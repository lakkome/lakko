// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

@testable import Lakko

/// `docs/ux/who-can-find-me.md` section 6.1 — one search field: when it asks
/// the network (three characters, 500 ms without typing, never twice for one
/// text in a session), what People says, and Add contact sending at once.
@MainActor
final class PeopleSearchFieldTests: XCTestCase {

    // MARK: - Fakes

    /// A debounce whose wait ends when the test says so.
    @MainActor
    private final class ManualSleeper {
        var waiting: [CheckedContinuation<Void, Never>] = []
        var requested: [UInt64] = []

        func wait(_ ms: UInt64) async {
            requested.append(ms)
            await withCheckedContinuation { waiting.append($0) }
        }

        func fire() {
            let woken = waiting
            waiting = []
            woken.forEach { $0.resume() }
        }
    }

    /// What the ports were asked.
    @MainActor
    private final class Recorder {
        var asked: [String] = []
        var sent: [Data] = []
        var answer: PeopleSearchAnswer = PeopleSearchAnswer(people: [])
        var sendError: Error?
        var local: [FoundPerson] = []
        var requested: Set<Data> = []
    }

    private struct Offline: Error {}

    private func person(_ seed: UInt8, name: String = "Anna Weber", reach: PersonReach? = nil) -> FoundPerson {
        FoundPerson(
            accountKey: Data(repeating: seed, count: 32), accountId: Data(repeating: seed, count: 16),
            displayName: name, username: "anna_w", writeSeed: nil, reach: reach)
    }

    private func session(_ rec: Recorder) -> PeopleSearchSession {
        PeopleSearchSession(ports: PeopleSearchSession.Ports(
            local: { _ in rec.local },
            network: { query in
                rec.asked.append(query)
                return rec.answer
            },
            sendRequest: { person in
                if let error = rec.sendError { throw error }
                rec.sent.append(person.accountKey)
            },
            alreadyRequested: { rec.requested.contains($0.accountKey) }))
    }

    private func instantModel(_ session: PeopleSearchSession) -> PeopleSearchModel {
        PeopleSearchModel(session: session, sleep: { _ in })
    }

    private func drain() async {
        for _ in 0..<20 { await Task.yield() }
    }

    private func type(_ text: String, into model: PeopleSearchModel) async {
        model.update(query: text)
        await model.settle()
    }

    // MARK: - When the network is asked

    func testTheNetworkIsAskedFromThreeCharactersWithoutTheAtSign() {
        XCTAssertFalse(PeopleSearch.asksNetwork(""))
        XCTAssertFalse(PeopleSearch.asksNetwork("an"))
        XCTAssertFalse(PeopleSearch.asksNetwork("@an"), "a leading @ is not a character")
        XCTAssertTrue(PeopleSearch.asksNetwork("ann"))
        XCTAssertTrue(PeopleSearch.asksNetwork("@ann"))
        XCTAssertTrue(PeopleSearch.asksNetwork("ли вэй"), "a name with a word of three letters")
        XCTAssertFalse(PeopleSearch.asksNetwork("a b c"), "no possible username, no three-letter word")
        XCTAssertFalse(PeopleSearch.asksNetwork("ли ли"))
        XCTAssertEqual(PeopleSearch.searchKey("@Anna"), PeopleSearch.searchKey("anna"))
        XCTAssertNil(PeopleSearch.searchKey("an"))
    }

    func testTheNetworkIsAskedOnlyAfterTypingStopsFor500Ms() async {
        let rec = Recorder()
        let sleeper = ManualSleeper()
        let model = PeopleSearchModel(session: session(rec), sleep: { ms in await sleeper.wait(ms) })

        model.update(query: "ann")
        await drain()
        model.update(query: "anna")
        await drain()
        XCTAssertEqual(rec.asked, [], "nothing is asked while the person is typing")
        XCTAssertNil(model.answer)
        XCTAssertFalse(model.searching)

        sleeper.fire()
        await model.settle()
        XCTAssertEqual(rec.asked, ["anna"], "only the text typing stopped at is asked")
        XCTAssertEqual(Set(sleeper.requested), [PeopleSearch.networkDebounceMs])
        XCTAssertEqual(PeopleSearch.networkDebounceMs, 500)
        XCTAssertNotNil(model.answer)
        XCTAssertFalse(model.searching)
    }

    func testFewerThanThreeCharactersAskNothingAndHidePeople() async {
        let rec = Recorder()
        let model = instantModel(session(rec))
        await type("anna", into: model)
        XCTAssertNotNil(model.answer)
        XCTAssertTrue(model.asksNetwork)
        await type("an", into: model)
        XCTAssertNil(model.answer, "the heading is hidden under three characters")
        XCTAssertFalse(model.asksNetwork)
        XCTAssertEqual(rec.asked, ["anna"])
        await type("a b c", into: model)
        XCTAssertEqual(rec.asked, ["anna"], "a text that cannot be a name asks nothing")
    }

    func testATextAskedOnceIsNotAskedAgainInTheSession() async {
        let rec = Recorder()
        rec.answer = PeopleSearchAnswer(people: [person(1)])
        let shared = session(rec)
        let chats = instantModel(shared)
        let contacts = instantModel(shared)

        await type("anna", into: chats)
        await type("annab", into: chats)
        await type("anna", into: chats)
        XCTAssertEqual(rec.asked, ["anna", "annab"])
        XCTAssertEqual(chats.answer?.people.map(\.accountKey), [person(1).accountKey])

        await type("@Anna", into: contacts)
        XCTAssertEqual(rec.asked, ["anna", "annab"], "the other field shares the session")
        XCTAssertNotNil(contacts.answer)
    }

    func testEarlierResultsStayUntilTheNewAnswerArrives() async {
        let rec = Recorder()
        rec.answer = PeopleSearchAnswer(people: [person(1)])
        let sleeper = ManualSleeper()
        let model = PeopleSearchModel(session: session(rec), sleep: { ms in await sleeper.wait(ms) })
        model.update(query: "anna")
        await drain()
        sleeper.fire()
        await model.settle()

        rec.answer = PeopleSearchAnswer(people: [person(2)])
        model.update(query: "anna w")
        await drain()
        XCTAssertEqual(model.answer?.people.map(\.accountKey), [person(1).accountKey])
        sleeper.fire()
        await model.settle()
        XCTAssertEqual(model.answer?.people.map(\.accountKey), [person(2).accountKey])
    }

    func testAFailedSearchIsAskedAgainWhenTypedAgain() async {
        let rec = Recorder()
        rec.answer = PeopleSearchAnswer(people: [], failed: true)
        let model = instantModel(session(rec))
        await type("anna", into: model)
        XCTAssertEqual(model.networkState(includesChannels: true), .failed)
        XCTAssertEqual(model.networkState(includesChannels: true)?.message,
                       Localization.text("search.people.failed"))
        await type("annab", into: model)
        await type("anna", into: model)
        XCTAssertEqual(rec.asked, ["anna", "annab", "anna"])
    }

    // MARK: - What People says

    func testOutOfTokensIsOutOfSearchesNeverNotFoundOrFailed() async {
        let rec = Recorder()
        rec.answer = PeopleSearchAnswer(people: [], allowanceUsed: true, failed: true)
        let model = instantModel(session(rec))
        await type("anna", into: model)
        XCTAssertEqual(model.networkState(includesChannels: true), .outOfSearches)
        XCTAssertEqual(model.networkState(includesChannels: true)?.message,
                       Localization.text("search.people.out_of_searches"))
        XCTAssertNotEqual(model.networkState(includesChannels: true)?.message,
                          Localization.text("search.people.not_found"))
        XCTAssertNotEqual(model.networkState(includesChannels: true)?.message,
                          Localization.text("search.people.failed"))
        XCTAssertEqual(PeopleSearch.message(for: .allowanceUsed),
                       Localization.text("search.people.out_of_searches"))
    }

    func testNotFoundOnlyWhenEveryQuestionWasAnswered() {
        XCTAssertEqual(PeopleSearchAnswer(people: []).state(includesChannels: true), .notFound)
        XCTAssertEqual(PeopleSearchAnswer(people: [], failed: true).state(includesChannels: true), .failed)
        XCTAssertEqual(PeopleSearchAnswer(people: [person(1)], failed: true).state(includesChannels: true), .found)
        XCTAssertFalse(PeopleSearchAnswer(people: [person(1)], failed: true).remembered)
        XCTAssertTrue(PeopleSearchAnswer(people: []).remembered)
    }

    /// A channel found by its handle is a result in Chats; Contacts matches
    /// only people.
    func testAChannelCountsInChatsOnly() {
        let answer = PeopleSearchAnswer(people: [], channel: FoundChannel(idHex: "ab", handle: "news"))
        XCTAssertEqual(answer.state(includesChannels: true), .found)
        XCTAssertEqual(answer.state(includesChannels: false), .notFound)
    }

    func testLocalPeopleComeFirstAndContactsHideTheirOwnContacts() async {
        let rec = Recorder()
        let contact = person(1, name: "Anna Contact", reach: .contact)
        let member = person(2, name: "Anna Member", reach: PersonReach.none)
        rec.local = [contact, member]
        // The network found the member as someone in Everyone, and a stranger.
        rec.answer = PeopleSearchAnswer(people: [person(2, reach: .open), person(3)])
        let model = instantModel(session(rec))
        await type("anna", into: model)

        let chats = model.rows(showsLocalContacts: true)
        XCTAssertEqual(chats.local.map(\.accountKey), [contact.accountKey, member.accountKey])
        XCTAssertEqual(chats.local.last?.reach, .open, "what the network learned is merged in")
        XCTAssertEqual(chats.network.map(\.accountKey), [person(3).accountKey])

        let contacts = model.rows(showsLocalContacts: false)
        XCTAssertEqual(contacts.local.map(\.accountKey), [member.accountKey])
    }

    // MARK: - Add contact

    func testAddContactSendsAtOnceAndTheRowReadsRequestSent() async {
        let rec = Recorder()
        let shared = session(rec)
        let anna = person(1)
        XCTAssertEqual(shared.requestState(for: anna), .idle)
        await shared.addContact(anna)
        XCTAssertEqual(rec.sent, [anna.accountKey])
        XCTAssertEqual(shared.requestState(for: anna), .sent)
        XCTAssertFalse(shared.requestState(for: anna).offersAdd, "the button goes away")
        XCTAssertNil(shared.requestState(for: anna).message)

        await shared.addContact(anna)
        XCTAssertEqual(rec.sent, [anna.accountKey], "a sent request is not sent twice")
    }

    func testARefusedRequestSaysTheyMayHaveChangedWhoCanFindThem() async {
        let rec = Recorder()
        rec.sendError = ContactsError.requestRefused
        let shared = session(rec)
        await shared.addContact(person(1))
        XCTAssertEqual(shared.requestState(for: person(1)), .refused)
        XCTAssertEqual(shared.requestState(for: person(1)).message,
                       Localization.text("search.people.request_refused"))
        XCTAssertFalse(shared.requestState(for: person(1)).offersAdd)
        XCTAssertEqual(PersonRequestState.after(error: ContactsError.wrapped(ContactsError.requestRefused)), .refused)
    }

    func testAnyOtherFailureBringsTheButtonBack() async {
        let rec = Recorder()
        rec.sendError = Offline()
        let shared = session(rec)
        await shared.addContact(person(1))
        XCTAssertEqual(shared.requestState(for: person(1)), .failed)
        XCTAssertEqual(shared.requestState(for: person(1)).message,
                       Localization.text("search.people.request_failed"))
        XCTAssertTrue(shared.requestState(for: person(1)).offersAdd)

        rec.sendError = nil
        await shared.addContact(person(1))
        XCTAssertEqual(shared.requestState(for: person(1)), .sent)
    }

    func testARequestAlreadySentReadsRequestSent() {
        let rec = Recorder()
        rec.requested = [person(1).accountKey]
        XCTAssertEqual(session(rec).requestState(for: person(1)), .sent)
    }

    func testSomeoneWhoCannotBeAddedIsNotSentARequest() async {
        let rec = Recorder()
        let shared = session(rec)
        await shared.addContact(person(1, reach: PersonReach.none))
        await shared.addContact(person(2, reach: .contact))
        XCTAssertEqual(rec.sent, [])
    }
}
