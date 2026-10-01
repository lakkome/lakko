// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

@testable import Lakko

/// `docs/ux/navigation.md` section 4.3 — the ONE network lookup.
///
/// The gate in front of it (`looks_like_a_name`) is `crates/lakko-ui-rules`
/// and is tested there. What is tested here is the part iOS owns: asking both
/// namespaces, and telling the difference between "no such name" and "no
/// answer at all".
final class NameLookupTests: XCTestCase {

    private struct Fake {
        var channel: Result<String?, Error> = .success(nil)
        var person: Result<String?, Error> = .success(nil)
        var lookups: NameLookups {
            let c = channel
            let p = person
            return NameLookups(
                channel: { _ in try c.get() },
                person: { _ in try p.get() })
        }
    }

    private struct Offline: Error {}

    // MARK: - The gate

    /// A name the blinder would refuse never costs a lookup: the alternative
    /// is making a person pay a proof of work to be told no.
    func testANameThatCannotBeCanonicalIsNeverAsked() async {
        var asked = false
        let lookups = NameLookups(
            channel: { _ in asked = true; return nil },
            person: { _ in asked = true; return nil })
        for typed in ["ab", "anna smith", "anna!", "", "@", String(repeating: "a", count: 33)] {
            let found = await NameLookup.find(typed: typed, lookups: lookups)
            XCTAssertEqual(found, .nothing, "asked about \(typed)")
        }
        XCTAssertFalse(asked, "a name the blinder refuses must not reach the network")
    }

    func testTheAtSignPeopleTypeIsStrippedBeforeAsking() async {
        var seen: [String] = []
        let lookups = NameLookups(
            channel: { seen.append($0); return nil },
            person: { seen.append($0); return nil })
        _ = await NameLookup.find(typed: "  @Anna  ", lookups: lookups)
        XCTAssertEqual(seen, ["Anna", "Anna"])
    }

    // MARK: - Two namespaces, one row

    func testAChannelHandleResolvesToAChannel() async {
        var fake = Fake()
        fake.channel = .success("ab12")
        let found = await NameLookup.find(typed: "@lakko_news", lookups: fake.lookups)
        XCTAssertEqual(found, .channel(channelIdHex: "ab12", handle: "lakko_news"))
    }

    /// The person namespace is asked even though the channel one answered
    /// first with nothing — the person typing does not know which namespace
    /// the name they were given lives in.
    func testAUsernameResolvesToAPersonAfterTheChannelNamespaceDeclines() async {
        var fake = Fake()
        fake.person = .success("anna")
        let found = await NameLookup.find(typed: "anna", lookups: fake.lookups)
        XCTAssertEqual(found, .person(username: "anna"))
    }

    func testBothNamespacesDecliningIsNothing() async {
        let found = await NameLookup.find(typed: "anna", lookups: Fake().lookups)
        XCTAssertEqual(found, .nothing)
    }

    // MARK: - The distinction that matters most

    /// Telling someone their friend does not exist because the connection
    /// dropped is the worst thing this can do.
    func testNeitherNamespaceReachableIsUnreachableRatherThanNothing() async {
        var fake = Fake()
        fake.channel = .failure(Offline())
        fake.person = .failure(Offline())
        let found = await NameLookup.find(typed: "anna", lookups: fake.lookups)
        XCTAssertEqual(found, .unreachable)
    }

    /// One namespace down and the other answering "no" is a real answer: the
    /// name is not a channel handle, and the service that would know said so.
    func testOneNamespaceDownAndTheOtherAnsweringIsNothing() async {
        var fake = Fake()
        fake.channel = .failure(Offline())
        let found = await NameLookup.find(typed: "anna", lookups: fake.lookups)
        XCTAssertEqual(found, .nothing)
    }

    /// A name that IS there is found even though the other namespace could not
    /// be reached at all.
    func testAFindSurvivesTheOtherNamespaceBeingDown() async {
        var fake = Fake()
        fake.channel = .failure(Offline())
        fake.person = .success("anna")
        let found = await NameLookup.find(typed: "anna", lookups: fake.lookups)
        XCTAssertEqual(found, .person(username: "anna"))
    }

    // MARK: - What it says

    func testTheTwoFailuresReadDifferently() {
        let nothing = NameLookup.message(for: .nothing, typed: "@anna")
        let unreachable = NameLookup.message(for: .unreachable, typed: "@anna")
        XCTAssertEqual(nothing?.withoutIsolates, "Nothing found for @anna.")
        XCTAssertEqual(
            unreachable, "Could not look that up. Check your connection and try again.")
        XCTAssertNotEqual(nothing, unreachable)
    }

    func testASuccessSaysNothingBecauseTheScreenNavigates() {
        XCTAssertNil(NameLookup.message(for: .person(username: "anna"), typed: "anna"))
        XCTAssertNil(
            NameLookup.message(for: .channel(channelIdHex: "ab", handle: "news"), typed: "news"))
    }

    /// The row offers the WHOLE name, never a prefix — the server answers the
    /// blinded name it was given and cannot answer "which names start with
    /// these letters".
    func testTheRowLabelNamesTheWholeThingOnce() {
        XCTAssertEqual(NameLookup.rowLabel(for: "  @Anna ").withoutIsolates, "Look up @Anna")
        XCTAssertEqual(NameLookup.rowLabel(for: "anna").withoutIsolates, "Look up @anna")
    }
}
