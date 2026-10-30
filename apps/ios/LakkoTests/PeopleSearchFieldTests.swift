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
        var candidates: [LocalCandidate] = []
        var requested: Set<Data> = []
        /// The seeds the reach check was asked about, in order.
        var checked: [Data] = []
        /// The answer of the reach check; `nil` throws.
        var reaches: (Data) -> Bool? = { _ in true }
        /// While set, every reach check waits for `releaseChecks()`.
        var holdChecks = false
        var heldChecks: [CheckedContinuation<Void, Never>] = []

        func releaseChecks() {
            holdChecks = false
            let held = heldChecks
            heldChecks = []
            held.forEach { $0.resume() }
        }
    }

    private struct Offline: Error {}

    private func person(_ seed: UInt8, name: String = "Anna Weber", reach: PersonReach? = nil) -> FoundPerson {
        FoundPerson(
            accountKey: Data(repeating: seed, count: 32), accountId: Data(repeating: seed, count: 16),
            displayName: name, username: "anna_w", writeSeed: nil, reach: reach)
    }

    private func kept(_ byte: UInt8) -> KeptGrant {
        let seed = Data(repeating: byte, count: 32)
        return KeptGrant(seed: seed, mailbox: MailboxTarget(
            mailboxPk: seed, address: seed, binding: Data(count: 97), accountKey: Data(count: 32)))
    }

    private func candidate(
        _ seed: UInt8, name: String = "Anna Weber", username: String? = "anna_w",
        isContact: Bool = false, grants: [KeptGrant] = []
    ) -> LocalCandidate {
        LocalCandidate(
            accountKey: Data(repeating: seed, count: 32), accountId: Data(repeating: seed, count: 16),
            known: KnownPerson(
                name: name, verifiedUsername: username, isContact: isContact, keptGrants: grants))
    }

    private func session(_ rec: Recorder) -> PeopleSearchSession {
        PeopleSearchSession(ports: PeopleSearchSession.Ports(
            candidates: { rec.candidates },
            ownAccountKey: { nil },
            network: { query in
                rec.asked.append(query)
                return rec.answer
            },
            reachCheck: { _, seed in
                rec.checked.append(seed)
                if rec.holdChecks {
                    await withCheckedContinuation { rec.heldChecks.append($0) }
                }
                guard let answer = rec.reaches(seed) else { throw Offline() }
                return answer
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
        rec.candidates = [
            candidate(1, name: "Anna Contact", isContact: true),
            candidate(2, name: "Anna Member"),
        ]
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

    // MARK: - A kept grant is checked before it is offered

    func testAStaleGrantThenALiveOneOffersTheLiveOne() async {
        let rec = Recorder()
        let (stale, live) = (kept(1), kept(2))
        rec.candidates = [candidate(4, grants: [stale, live])]
        rec.reaches = { $0 == live.seed }
        let model = instantModel(session(rec))
        await type("anna", into: model)

        let row = model.local.first
        XCTAssertEqual(rec.checked, [stale.seed, live.seed])
        XCTAssertEqual(row?.writeSeed, live.seed)
        XCTAssertEqual(row?.mailbox, live.mailbox)
        XCTAssertEqual(row?.username, "anna_w")
        XCTAssertEqual(row?.canAddContact, true)
        XCTAssertEqual(row?.reachChecked, true)
    }

    func testAllGrantsRefusedLeaveNoButtonNoUsernameAndNoMatchByUsername() async {
        let rec = Recorder()
        rec.candidates = [candidate(4, grants: [kept(1), kept(2)])]
        rec.reaches = { _ in false }
        let model = instantModel(session(rec))
        await type("anna_w", into: model)
        XCTAssertTrue(model.local.isEmpty, "a dead person is matched by name only")

        await type("anna", into: model)
        let row = model.local.first
        XCTAssertEqual(row?.reach, PersonReach.none)
        XCTAssertNil(row?.username)
        XCTAssertEqual(row?.canAddContact, false)
        XCTAssertEqual(rec.checked, [kept(1).seed, kept(2).seed], "a verdict is not asked for again")
    }

    func testACheckThatThrowsRecordsNoVerdictAndTheRowBehavesAsBefore() async {
        let rec = Recorder()
        rec.candidates = [candidate(4, grants: [kept(1), kept(2)])]
        rec.reaches = { _ in nil }
        let session = session(rec)
        let model = instantModel(session)
        await type("anna", into: model)

        XCTAssertTrue(session.verdicts.isEmpty)
        let row = model.local.first
        XCTAssertEqual(row?.writeSeed, kept(1).seed)
        XCTAssertEqual(row?.username, "anna_w")
        XCTAssertEqual(row?.canAddContact, true)
        XCTAssertEqual(row?.reachChecked, false)

        // The next text asks again; the answer now arrives.
        rec.reaches = { _ in false }
        await type("anna w", into: model)
        XCTAssertNotNil(session.verdicts[row?.accountKey ?? Data()])
        XCTAssertEqual(model.local.first?.canAddContact, false)
    }

    func testAContactIsNeverCheckedAndNeverWaits() async {
        let rec = Recorder()
        rec.candidates = [candidate(4, isContact: true, grants: [kept(1)])]
        let model = instantModel(session(rec))
        model.update(query: "anna")
        XCTAssertEqual(model.local.first?.reach, .contact)
        XCTAssertEqual(model.local.first?.username, "anna_w")
        await model.settle()
        XCTAssertTrue(rec.checked.isEmpty)
    }

    /// Before the verdict: the name, no button, no username. From the first
    /// letter, before the network debounce, and with no token.
    func testARowWaitingForItsVerdictShowsItsNameAlone() async {
        let rec = Recorder()
        rec.holdChecks = true
        rec.candidates = [candidate(4, grants: [kept(1)])]
        let sleeper = ManualSleeper()
        let model = PeopleSearchModel(session: session(rec), sleep: { ms in await sleeper.wait(ms) })
        model.update(query: "a")
        await drain()

        XCTAssertEqual(rec.checked, [kept(1).seed], "no 3-character minimum and no debounce")
        XCTAssertTrue(sleeper.requested.isEmpty)
        XCTAssertEqual(model.local.first?.displayName, "Anna Weber")
        XCTAssertEqual(model.local.first?.canAddContact, false)
        XCTAssertNil(model.local.first?.username)

        rec.releaseChecks()
        await model.settle()
        XCTAssertEqual(model.local.first?.canAddContact, true)
        XCTAssertEqual(model.local.first?.username, "anna_w")
    }

    func testOnePersonIsNotCheckedTwiceAtOnce() async {
        let rec = Recorder()
        rec.holdChecks = true
        rec.candidates = [candidate(4, grants: [kept(1)])]
        let session = session(rec)
        let chats = instantModel(session)
        let contacts = instantModel(session)
        chats.update(query: "an")
        await drain()
        chats.update(query: "ann")
        contacts.update(query: "an")
        await drain()
        XCTAssertEqual(rec.checked.count, 1)

        rec.releaseChecks()
        await chats.settle()
        await contacts.settle()
        XCTAssertEqual(rec.checked.count, 1)
        XCTAssertEqual(chats.local.first?.canAddContact, true, "both fields take the verdict")
        XCTAssertEqual(contacts.local.first?.canAddContact, true)
    }

    func testOneLetterDoesNotAskAboutEveryoneAtOnce() async {
        let rec = Recorder()
        rec.holdChecks = true
        rec.candidates = (10..<20).map { candidate(UInt8($0), grants: [kept(UInt8($0))]) }
        let model = instantModel(session(rec))
        model.update(query: "a")
        await drain()
        XCTAssertEqual(rec.checked.count, PeopleSearchSession.maxConcurrentReachChecks)

        rec.releaseChecks()
        await model.settle()
        XCTAssertEqual(rec.checked.count, 10)
        XCTAssertEqual(model.local.filter(\.canAddContact).count, 10)
    }

    func testAGrantReceivedAfterADeadVerdictIsCheckedAgain() async {
        let rec = Recorder()
        rec.candidates = [candidate(4, grants: [kept(1)])]
        rec.reaches = { _ in false }
        let model = instantModel(session(rec))
        await type("anna", into: model)
        XCTAssertEqual(model.local.first?.canAddContact, false)

        rec.candidates = [candidate(4, grants: [kept(1), kept(2)])]
        let live = kept(2).seed
        rec.reaches = { $0 == live }
        await type("anna w", into: model)
        XCTAssertEqual(model.local.first?.canAddContact, true)
        XCTAssertEqual(model.local.first?.writeSeed, kept(2).seed)
    }

    func testARefusedSendTakesTheButtonAndKeepsTheText() async {
        let rec = Recorder()
        rec.candidates = [candidate(4, grants: [kept(1), kept(2)])]
        rec.sendError = ContactsError.requestRefused
        let session = session(rec)
        let model = instantModel(session)
        await type("anna", into: model)
        let row = try? XCTUnwrap(model.local.first)
        XCTAssertEqual(row?.canAddContact, true)

        await session.addContact(row!)

        let after = model.local.first
        XCTAssertEqual(after?.canAddContact, false)
        XCTAssertNil(after?.username)
        let state = session.requestState(for: after!)
        XCTAssertEqual(state, .refused)
        XCTAssertEqual(state.message(canAddContact: false),
                       Localization.text("search.people.request_refused"))

        // Neither grant is asked about again for this person.
        rec.checked = []
        await type("anna w", into: model)
        XCTAssertTrue(rec.checked.isEmpty)
    }

    func testAnOrdinaryFailureRecordsNoVerdict() async {
        let rec = Recorder()
        rec.candidates = [candidate(4, grants: [kept(1)])]
        rec.sendError = Offline()
        let session = session(rec)
        let model = instantModel(session)
        await type("anna", into: model)
        await session.addContact(model.local[0])
        XCTAssertEqual(model.local.first?.canAddContact, true)
        XCTAssertEqual(session.requestState(for: model.local[0]), .failed)
    }

    /// The local row and a network result of the same person: the grant the
    /// network just checked replaces a kept one no check could answer for.
    func testAGrantTheNetworkCheckedBeatsOneThatWasNotAnswered() async {
        let rec = Recorder()
        rec.candidates = [candidate(4, grants: [kept(1)])]
        rec.reaches = { _ in nil }
        let live = kept(2)
        rec.answer = PeopleSearchAnswer(people: [FoundPerson(
            accountKey: Data(repeating: 4, count: 32), accountId: Data(repeating: 4, count: 16),
            displayName: "Published", username: "anna_w", writeSeed: live.seed, reach: .grant,
            mailbox: live.mailbox
        ).passingReachCheck()])
        let model = instantModel(session(rec))
        await type("anna", into: model)

        let rows = model.rows(showsLocalContacts: true)
        XCTAssertEqual(rows.local.first?.writeSeed, live.seed)
        XCTAssertEqual(rows.local.first?.displayName, "Anna Weber")
    }

    // MARK: - "No one found"

    func testNoOneFoundIsHiddenWhileAPersonRowStands() async {
        let rec = Recorder()
        rec.candidates = [candidate(4, grants: [kept(1)])]
        rec.reaches = { _ in false }
        let model = instantModel(session(rec))
        await type("anna", into: model)

        XCTAssertEqual(model.networkState(includesChannels: true), .notFound)
        XCTAssertEqual(model.rows(showsLocalContacts: true).local.count, 1)
        XCTAssertNil(model.peopleMessage(includesChannels: true, showsLocalContacts: true))
    }

    func testNoOneFoundShowsWhenNoPersonRowStands() async {
        let rec = Recorder()
        let model = instantModel(session(rec))
        await type("anna", into: model)
        XCTAssertEqual(model.peopleMessage(includesChannels: true, showsLocalContacts: true),
                       Localization.text("search.people.not_found"))

        // A contact the Contacts list shows itself is no row of this section.
        rec.candidates = [candidate(4, isContact: true)]
        await type("anna w", into: model)
        XCTAssertEqual(model.peopleMessage(includesChannels: false, showsLocalContacts: false),
                       Localization.text("search.people.not_found"))
    }

    func testAFailedSearchStillSaysSoBesideARow() async {
        let rec = Recorder()
        rec.candidates = [candidate(4)]
        rec.answer = PeopleSearchAnswer(people: [], failed: true)
        let model = instantModel(session(rec))
        await type("anna", into: model)
        XCTAssertEqual(model.peopleMessage(includesChannels: true, showsLocalContacts: true),
                       Localization.text("search.people.failed"))
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
