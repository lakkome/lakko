// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

@testable import Lakko

/// The values every call link carries (`docs/ux/call-links.md` section 4).
///
/// These used to be four questions on a form, answered differently by three
/// clients — two of which disagreed about what one of the answers meant. They
/// are constants now, and these are what pin them: a change to any of them is
/// a decision someone has to make on purpose.
final class CallInviteValuesTests: XCTestCase {

    func testOneKindOfCall() {
        XCTAssertTrue(CallInviteValues.audio)
        XCTAssertTrue(CallInviteValues.video, "one kind of call: group, video")
    }

    func testTheLinkIsUnboundedAndUnlabelled() {
        XCTAssertEqual(
            CallInviteValues.maxUses, UInt32.max,
            "the host admits each arrival, so a count here would cap a meeting for no gain")
        XCTAssertEqual(CallInviteValues.label, "")
    }

    /// The one value the expiry must never take. `0` is `CALL_NEVER_EXPIRES`,
    /// and a link that works forever is the stalking risk that field's own
    /// documentation names.
    func testTheExpiryIsNeverTheNeverExpiresSentinel() {
        for now in [UInt64(0), 1, 1_700_000_000_000, UInt64.max / 2, UInt64.max] {
            XCTAssertNotEqual(CallInviteValues.expiryMs(nowMs: now), 0, "now=\(now)")
        }
    }

    /// The WINDOW is not asserted here, and its absence is the point. It lives
    /// in `lakko_contacts::call_invite`, and the two bounds it satisfies - long
    /// enough to outlast a call, gone by the next day - are `const` assertions
    /// in Rust, so a value outside them fails the build rather than a test.
    /// Restating a number this client no longer owns is what let it ship twelve
    /// hours while Android shipped twenty-four.
    func testTheExpiryIsAlwaysInTheFuture() {
        let now: UInt64 = 1_700_000_000_000
        XCTAssertGreaterThan(CallInviteValues.expiryMs(nowMs: now), now)
    }

    /// A clock far enough in the future would wrap the window back past zero,
    /// and zero is the one forbidden value.
    func testAnAbsurdClockSaturatesRatherThanWrapping() {
        XCTAssertEqual(CallInviteValues.expiryMs(nowMs: UInt64.max), UInt64.max)
    }

    func testAFreshCallIdIsSixteenRandomBytes() {
        let a = CallInviteValues.freshCallId()
        let b = CallInviteValues.freshCallId()
        XCTAssertEqual(a.count, CallInviteValues.callIdLen)
        XCTAssertEqual(b.count, 16)
        XCTAssertNotEqual(a, b)
        XCTAssertFalse(a.allSatisfy { $0 == 0 })
    }
}

/// What one use of `Invite` does (`docs/ux/call-links.md` sections 3 and 5).
/// The same list as the web `in-call-invite.test.ts`.
final class CallInviteOfferRulesTests: XCTestCase {

    typealias Rules = CallInviteOfferRules

    private let url = "https://lakko.me/c/AAAA"

    // MARK: nextPress

    /// The first use in a call is what pays for the link, which is what keeps
    /// starting a call instant; a second use costs no round trip.
    func testMintsOnceAndThenReOffers() {
        XCTAssertEqual(Rules.nextPress(.idle), .mint)
        XCTAssertEqual(Rules.nextPress(.ready(url: url, delivery: .copied)), .reoffer)
    }

    /// Two mints for one call would produce two different links for it, and a
    /// tap that lands while the first is in flight is the ordinary way to get
    /// there.
    func testAPressWhileOneIsBeingMadeIsIgnored() {
        XCTAssertEqual(Rules.nextPress(.minting), .ignore)
    }

    func testAFailedMintCanBeTriedAgain() {
        XCTAssertEqual(Rules.nextPress(.failed(failure: .unavailable, url: nil)), .mint)
        XCTAssertEqual(Rules.nextPress(.failed(failure: .signedOut, url: nil)), .mint)
    }

    /// The link exists; only the delivery failed. The next use sends the same
    /// link rather than minting a second one for the same call.
    func testDoesNotMintAgainWhenOnlyTheSendingFailed() {
        let state = CallInviteOfferState.failed(failure: .notSent, url: url)
        XCTAssertEqual(Rules.nextPress(state), .reoffer)
        XCTAssertEqual(Rules.mintedUrl(state), url)
        XCTAssertNil(Rules.mintedUrl(.failed(failure: .unavailable, url: nil)))
        XCTAssertNil(Rules.mintedUrl(.idle))
    }

    // MARK: busy

    func testBusyOnlyWhileMinting() {
        XCTAssertFalse(Rules.busy(.idle))
        XCTAssertTrue(Rules.busy(.minting))
        XCTAssertFalse(Rules.busy(.ready(url: url, delivery: .copied)))
        XCTAssertFalse(Rules.busy(.failed(failure: .unavailable, url: nil)))
    }

    // MARK: statusLine

    func testSaysNothingBeforeTheFirstPress() {
        XCTAssertNil(Rules.statusLine(.idle))
    }

    /// Section 5: the busy state "says what it is doing rather than only
    /// spinning".
    func testSaysWhatTheWaitIsNotOnlyThatThereIsOne() {
        let line = Rules.statusLine(.minting) ?? ""
        XCTAssertTrue(line.contains("link"))
        XCTAssertTrue(line.contains("few seconds"))
    }

    func testNamesWhereTheLinkWent() {
        XCTAssertEqual(
            Rules.statusLine(.ready(url: url, delivery: .sent(name: "Anna")))
                .map { $0.withoutIsolates },
            "Invitation sent to Anna.")
        XCTAssertTrue(
            Rules.statusLine(.ready(url: url, delivery: .copied))?.contains("copied") == true)
        XCTAssertTrue(Rules.statusLine(.ready(url: url, delivery: .shown))?.contains(url) == true)
    }

    /// Section 5: "One sentence on what happened and that it can be tried
    /// again. The call is unaffected and does not end."
    func testAFailureSaysWhatHappenedAndWhatToDo() {
        let failed = Rules.statusLine(.failed(failure: .unavailable, url: nil)) ?? ""
        XCTAssertTrue(failed.contains("could not"))
        XCTAssertTrue(failed.contains("try again"))
        XCTAssertTrue(failed.contains("call is unaffected"))
        XCTAssertTrue(
            Rules.statusLine(.failed(failure: .signedOut, url: nil))?.contains("Sign in again")
                == true)
        XCTAssertEqual(
            Rules.statusLine(.failed(failure: .notSent, url: url)),
            Localization.text("call.invite.send_failed"))
    }

    /// Rendered from the kind of failure (ADR-0061): the failure's own prose
    /// never reaches the screen.
    func testAMintFailureIsRenderedFromItsKind() {
        struct Refused: Error {}
        XCTAssertEqual(Rules.failure(for: Refused()), .unavailable)
        XCTAssertEqual(Rules.failure(for: LinkCallHostError.signedOut), .signedOut)
        XCTAssertEqual(
            Rules.failureMessage(for: Refused()), Localization.text("call.invite.unavailable"))
        XCTAssertEqual(
            Rules.failureMessage(for: LinkCallHostError.signedOut), Rules.signedOutMessage)
    }

    /// The copy rules: no protocol names anywhere a person can read, and no
    /// exclamation marks.
    ///
    /// Matched on whole words, because the acronyms are short enough to hide
    /// inside ordinary English — a substring check for `DS` fails on
    /// "seconds", which is a word the busy line has every right to use.
    func testNoProtocolVocabularyReachesTheScreen() {
        let banned = [
            "MLS", "OPRF", "KeyPackage", "pseudonym", "epoch", "capability",
            "proof of work", "proof-of-work", "by design", "DS",
        ]
        let states: [CallInviteOfferState] = [
            .idle, .minting,
            .ready(url: url, delivery: .sent(name: "Anna")),
            .failed(failure: .notSent, url: url),
            .ready(url: url, delivery: .copied),
            .ready(url: url, delivery: .shown),
            .failed(failure: .signedOut, url: nil),
            .failed(failure: .unavailable, url: nil),
        ]
        let copy = states.compactMap(Rules.statusLine) + [Rules.controlLabel]
        for line in copy {
            for word in banned {
                let pattern = "\\b\(NSRegularExpression.escapedPattern(for: word))\\b"
                let hit = line.range(of: pattern, options: [.regularExpression, .caseInsensitive])
                XCTAssertNil(hit, "\"\(word)\" must not reach the screen: \(line)")
            }
            XCTAssertFalse(line.contains("!"), line)
        }
    }

    // MARK: statusHoldMs

    func testHoldsAFinishedLineForAFewSeconds() {
        XCTAssertEqual(
            Rules.statusHoldMs(.ready(url: url, delivery: .copied)), Rules.statusHoldMs)
        XCTAssertEqual(
            Rules.statusHoldMs(.failed(failure: .unavailable, url: nil)), Rules.statusHoldMs)
        XCTAssertEqual(Rules.statusHoldMs, 6_000)
    }

    func testNeverTimesOutAWaitAnEmptyLineOrALinkBeingCopiedByHand() {
        XCTAssertNil(Rules.statusHoldMs(.minting))
        XCTAssertNil(Rules.statusHoldMs(.idle))
        XCTAssertNil(Rules.statusHoldMs(.ready(url: url, delivery: .shown)))
    }

    // MARK: copyLink

    func testPutsTheLinkOnTheClipboard() {
        var copied: [String] = []
        XCTAssertEqual(Rules.copyLink(url, copy: { copied.append($0) }), .copied)
        XCTAssertEqual(copied, [url])
    }

    func testShowsTheLinkWhenTheClipboardIsNotReachable() {
        struct Denied: Error {}
        XCTAssertEqual(Rules.copyLink(url, copy: nil), .shown)
        XCTAssertEqual(Rules.copyLink(url, copy: { _ in throw Denied() }), .shown)
    }
}

/// The offer itself, against a mint, a clipboard and a send path that never
/// touch a network.
@MainActor
final class CallInviteOfferTests: XCTestCase {

    private final class World {
        var mints = 0
        var url = "https://lakko.me/c/one#secret"
        var mintFailure: Error?
        var clipboard: [String] = []
        var sent: [(conversation: String, text: String)] = []
        var sendFailure: Error?
        var contacts: [InviteContactRow] = []
        var holds: [UInt64] = []
    }

    private struct Boom: Error, LocalizedError {
        var errorDescription: String? { "the upload was refused" }
    }

    private func row(_ key: String, _ name: String) -> InviteContactRow {
        InviteContactRow(key: Data(key.utf8), name: name, username: nil)
    }

    private func makeOffer(_ world: World) -> CallInviteOffer {
        CallInviteOffer(
            mint: {
                world.mints += 1
                if let failure = world.mintFailure { throw failure }
                return world.url
            },
            copy: { world.clipboard.append($0) },
            contacts: { world.contacts },
            send: { conversation, text in
                if let failure = world.sendFailure { throw failure }
                world.sent.append((conversation, text))
                return .sent
            },
            // The line's time on screen is not waited out in a test.
            hold: { ms in world.holds.append(ms) })
    }

    func testACallStartsWithNoLinkAtAll() {
        let offer = makeOffer(World())
        XCTAssertEqual(offer.state, .idle)
        XCTAssertNil(offer.url)
        XCTAssertFalse(offer.menuOpen)
        XCTAssertFalse(offer.pickerOpen)
        XCTAssertNil(offer.statusLine)
    }

    /// `Participants` opens the app's own menu and does nothing else: no link
    /// is made until an invite item is chosen.
    func testParticipantsOpensTheMenuAndMintsNothing() {
        let world = World()
        let offer = makeOffer(world)
        offer.toggleMenu()
        XCTAssertTrue(offer.menuOpen)
        XCTAssertEqual(world.mints, 0)
        offer.toggleMenu()
        XCTAssertFalse(offer.menuOpen)
    }

    func testCopyLinkMintsCopiesAndSaysSo() async {
        let world = World()
        let offer = makeOffer(world)
        offer.toggleMenu()
        await offer.copyLink()
        XCTAssertEqual(world.mints, 1)
        XCTAssertEqual(world.clipboard, [world.url], "the fragment travels with the link")
        XCTAssertEqual(offer.state, .ready(url: world.url, delivery: .copied))
        XCTAssertFalse(offer.menuOpen, "choosing an item closes the menu")
        XCTAssertEqual(offer.statusLine, Localization.text("call.invite.copied"))
    }

    /// The whole point of holding the offer on the call: the same link for the
    /// whole call, and no second round trip to hand it out again.
    func testASecondUseReOffersTheSameLinkWithoutMintingAgain() async {
        let world = World()
        let offer = makeOffer(world)
        await offer.copyLink()
        world.url = "https://lakko.me/c/two#other"
        await offer.copyLink()
        XCTAssertEqual(offer.url, "https://lakko.me/c/one#secret")
        XCTAssertEqual(world.mints, 1)
        XCTAssertEqual(world.clipboard.count, 2)
    }

    func testAFailedMintIsExplainedAndCanBeTriedAgain() async {
        let world = World()
        world.mintFailure = Boom()
        let offer = makeOffer(world)
        await offer.copyLink()
        XCTAssertEqual(offer.state, .failed(failure: .unavailable, url: nil))
        let line = offer.statusLine ?? ""
        // Rendered from the kind; the error's own prose never reaches the screen.
        XCTAssertFalse(line.contains("the upload was refused"))
        XCTAssertTrue(line.contains("The call is unaffected"))
        XCTAssertEqual(world.clipboard, [])

        world.mintFailure = nil
        await offer.copyLink()
        XCTAssertEqual(offer.state, .ready(url: world.url, delivery: .copied))
        XCTAssertEqual(world.mints, 2)
    }

    // MARK: Invite a contact

    /// The list is the person's contacts, read each time it is opened. It is
    /// empty for someone with none; the item is offered all the same
    /// (`CallInviteContactsItemTests`).
    func testInviteContactsListsTheContacts() {
        let world = World()
        let offer = makeOffer(world)
        XCTAssertTrue(offer.targets().isEmpty)
        world.contacts = [row("aa", "Anna")]
        XCTAssertEqual(offer.targets().map(\.name), ["Anna"])
        XCTAssertEqual(offer.targets().map(\.key), [Data("aa".utf8)])
    }

    /// The message is the call's link and nothing else, sent through the
    /// conversation's own send path. The picker STAYS OPEN so several people
    /// can be invited; the row is marked as invited and the line names the
    /// conversation.
    func testInvitingAContactSendsTheLinkAndNothingElse() async {
        let world = World()
        world.contacts = [row("aa", "Anna")]
        let offer = makeOffer(world)
        offer.toggleMenu()
        offer.openPicker()
        XCTAssertFalse(offer.menuOpen)
        XCTAssertTrue(offer.pickerOpen)

        let sent = await offer.sendInvite(to: CallInviteTarget(id: "aa", title: "Anna"))
        XCTAssertTrue(sent)
        XCTAssertEqual(world.sent.count, 1)
        XCTAssertEqual(world.sent.first?.conversation, "aa")
        XCTAssertEqual(world.sent.first?.text, world.url, "the whole text is the link")
        XCTAssertTrue(offer.pickerOpen, "the list stays open for the next person")
        XCTAssertEqual(offer.invited, ["aa"])
        XCTAssertNil(offer.pickerError)
        XCTAssertEqual(offer.state, .ready(url: world.url, delivery: .sent(name: "Anna")))
        XCTAssertEqual(offer.statusLine?.withoutIsolates, "Invitation sent to Anna.")
        XCTAssertEqual(world.clipboard, [], "sending is not copying")
    }

    /// Several people in one go: each send reuses the one link, each row is
    /// marked, and an invited row cannot be picked a second time.
    func testSeveralContactsAreInvitedWithOneLinkAndNoneTwice() async {
        let world = World()
        world.contacts = [row("aa", "Anna"), row("bb", "Boris")]
        let offer = makeOffer(world)
        offer.openPicker()
        _ = await offer.sendInvite(to: CallInviteTarget(id: "aa", title: "Anna"))
        _ = await offer.sendInvite(to: CallInviteTarget(id: "bb", title: "Boris"))
        let again = await offer.sendInvite(to: CallInviteTarget(id: "aa", title: "Anna"))
        XCTAssertFalse(again, "an invited row is not sent to twice")
        XCTAssertEqual(world.sent.map(\.conversation), ["aa", "bb"])
        XCTAssertEqual(Set(world.sent.map(\.text)), [world.url])
        XCTAssertEqual(world.mints, 1)
        XCTAssertEqual(offer.invited, ["aa", "bb"])
        XCTAssertEqual(offer.statusLine?.withoutIsolates, "Invitation sent to Boris.")
        XCTAssertTrue(offer.pickerOpen)
        offer.closePicker()
        XCTAssertFalse(offer.pickerOpen)
    }

    /// A failure keeps the list open with the reason and the row pressable,
    /// the call is untouched, and the link already made is kept: the next
    /// attempt must not mint a second link for the same call.
    func testAFailedSendKeepsThePickerOpenAndDoesNotMintAgain() async {
        let world = World()
        world.contacts = [row("aa", "Anna"), row("bb", "Boris")]
        world.sendFailure = Boom()
        let offer = makeOffer(world)
        offer.openPicker()

        let first = await offer.sendInvite(to: CallInviteTarget(id: "aa", title: "Anna"))
        XCTAssertFalse(first)
        XCTAssertTrue(offer.pickerOpen, "the list stays open with the reason")
        XCTAssertEqual(offer.pickerError, Localization.text("call.invite.send_failed"))
        XCTAssertEqual(offer.state, .failed(failure: .notSent, url: world.url))
        XCTAssertEqual(world.mints, 1)
        XCTAssertTrue(offer.invited.isEmpty, "a row that was not sent to stays pressable")

        world.sendFailure = nil
        world.url = "https://lakko.me/c/two#other"
        let second = await offer.sendInvite(to: CallInviteTarget(id: "aa", title: "Anna"))
        XCTAssertTrue(second)
        XCTAssertEqual(world.mints, 1, "a failed send does not mint again")
        XCTAssertEqual(world.sent.first?.text, "https://lakko.me/c/one#secret")
        XCTAssertEqual(offer.invited, ["aa"])
        XCTAssertNil(offer.pickerError)
    }

    /// When the link cannot be made at all, nothing is sent and the picker
    /// says so.
    func testASendWithNoLinkToSendSaysSoAndSendsNothing() async {
        let world = World()
        world.mintFailure = Boom()
        let offer = makeOffer(world)
        offer.openPicker()
        let sent = await offer.sendInvite(to: CallInviteTarget(id: "aa", title: "Anna"))
        XCTAssertFalse(sent)
        XCTAssertTrue(world.sent.isEmpty)
        XCTAssertTrue(offer.pickerOpen)
        XCTAssertEqual(offer.pickerError, Localization.text("call.invite.send_failed"))
    }

    func testClosingThePickerSendsNothingAndClearsItsReason() async {
        let world = World()
        world.sendFailure = Boom()
        let offer = makeOffer(world)
        offer.openPicker()
        _ = await offer.sendInvite(to: CallInviteTarget(id: "aa", title: "Anna"))
        offer.closePicker()
        XCTAssertFalse(offer.pickerOpen)
        XCTAssertNil(offer.pickerError)
        XCTAssertTrue(world.sent.isEmpty)
    }

    /// A finished line is asked to hold for a few seconds; a wait is not.
    func testAFinishedLineIsHeldAndThenGoes() async {
        let world = World()
        let offer = makeOffer(world)
        await offer.copyLink()
        // The hold runs as its own task; let it finish.
        for _ in 0..<20 where offer.statusLine != nil { await Task.yield() }
        XCTAssertEqual(world.holds, [CallInviteOfferRules.statusHoldMs])
        XCTAssertNil(offer.statusLine, "the line goes once its time is up")
        XCTAssertEqual(offer.url, world.url, "the link itself is kept")
    }
}
