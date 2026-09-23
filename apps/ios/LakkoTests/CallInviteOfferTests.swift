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

/// What one press of `Invite` does.
final class CallInviteOfferRulesTests: XCTestCase {

    typealias Rules = CallInviteOfferRules

    /// The first press in a call is what pays for the link — which is what
    /// keeps starting a call instant.
    func testTheFirstPressMints() {
        XCTAssertEqual(Rules.press(.idle), .mint)
    }

    /// "A second `Invite` in the same call re-offers the link already minted."
    func testASecondPressReOffersRatherThanMintingASecondLink() {
        XCTAssertEqual(Rules.press(.ready(url: "https://lakko.me/c/abc")), .offer(url: "https://lakko.me/c/abc"))
    }

    /// Two mints for one call would produce two different links for it, and a
    /// tap that lands while the first is in flight is the ordinary way to get
    /// there.
    func testAPressWhileOneIsBeingMadeIsIgnored() {
        XCTAssertEqual(Rules.press(.minting), .ignore)
    }

    func testAFailedAttemptCanBeRetried() {
        XCTAssertEqual(Rules.press(.failed(message: "nope")), .mint)
    }

    /// Section 5: "One sentence on what happened and that it can be tried
    /// again. The call is unaffected and does not end."
    func testTheFailureSaysWhatHappenedAndThatTheCallIsStillRunning() {
        let message = Rules.failureMessage("the server said no")
        XCTAssertTrue(message.contains("the server said no"))
        XCTAssertTrue(message.contains("The call is still running"))
        XCTAssertTrue(message.contains("try again"))
        XCTAssertFalse(message.contains("!"))
    }

    func testAFailureWithNoReasonStillSaysSomething() {
        let message = Rules.failureMessage("   ")
        XCTAssertEqual(message, "The link could not be made. The call is still running — try again.")
    }

    /// The copy rules: no protocol names anywhere a person can read.
    ///
    /// Matched on whole words, because the acronyms are short enough to hide
    /// inside ordinary English — a substring check for `DS` fails on
    /// "seconds", which is a word the busy line has every right to use.
    func testNoProtocolVocabularyReachesTheScreen() {
        let banned = [
            "MLS", "OPRF", "KeyPackage", "pseudonym", "epoch", "capability",
            "proof of work", "proof-of-work", "by design", "DS",
        ]
        let copy = [
            Rules.controlLabel, Rules.mintingMessage, Rules.readyMessage,
            Rules.signedOutMessage, Rules.failureMessage("timed out"),
        ]
        for line in copy {
            for word in banned {
                let pattern = "\\b\(NSRegularExpression.escapedPattern(for: word))\\b"
                let hit = line.range(of: pattern, options: [.regularExpression, .caseInsensitive])
                XCTAssertNil(hit, "\"\(word)\" must not reach the screen: \(line)")
            }
            XCTAssertFalse(line.contains("!"), line)
        }
    }

    /// Section 5: the busy state "says what it is doing rather than only
    /// spinning".
    func testTheBusyStateSaysWhatItIsDoing() {
        XCTAssertTrue(Rules.mintingMessage.hasPrefix("Making a link"))
        XCTAssertTrue(Rules.mintingMessage.contains("seconds"))
    }
}

/// The offer itself, against a mint that never touches a network.
@MainActor
final class CallInviteOfferTests: XCTestCase {

    private final class Mints {
        var calls = 0
        var url = "https://lakko.me/c/one"
        var failure: Error?
    }

    private func makeOffer(_ mints: Mints) -> CallInviteOffer {
        CallInviteOffer(mint: {
            mints.calls += 1
            if let failure = mints.failure { throw failure }
            return mints.url
        })
    }

    private struct Boom: Error, LocalizedError {
        var errorDescription: String? { "the upload was refused" }
    }

    func testACallStartsWithNoLinkAtAll() {
        let offer = makeOffer(Mints())
        XCTAssertEqual(offer.state, .idle)
        XCTAssertNil(offer.url)
        XCTAssertFalse(offer.panelOpen)
    }

    func testTheFirstPressMakesALinkAndOpensThePanel() async {
        let mints = Mints()
        let offer = makeOffer(mints)
        await offer.press()
        XCTAssertEqual(offer.state, .ready(url: "https://lakko.me/c/one"))
        XCTAssertEqual(offer.url, "https://lakko.me/c/one")
        XCTAssertTrue(offer.panelOpen, "the work is shown, not done off-screen")
        XCTAssertEqual(mints.calls, 1)
    }

    /// The whole point of holding the offer on the call: the same link for the
    /// whole call, and no second round trip to re-show it.
    func testASecondPressReOffersTheSameLinkWithoutMintingAgain() async {
        let mints = Mints()
        let offer = makeOffer(mints)
        await offer.press()
        offer.setPanelOpen(false)
        mints.url = "https://lakko.me/c/two"
        await offer.press()
        XCTAssertEqual(offer.url, "https://lakko.me/c/one")
        XCTAssertEqual(mints.calls, 1)
        XCTAssertTrue(offer.panelOpen)
    }

    func testAFailureIsExplainedAndTheOfferStaysUsable() async {
        let mints = Mints()
        mints.failure = Boom()
        let offer = makeOffer(mints)
        await offer.press()
        guard case .failed(let message) = offer.state else {
            return XCTFail("expected a failure, got \(offer.state)")
        }
        XCTAssertTrue(message.contains("the upload was refused"))
        XCTAssertTrue(message.contains("The call is still running"))

        mints.failure = nil
        await offer.retry()
        XCTAssertEqual(offer.state, .ready(url: "https://lakko.me/c/one"))
        XCTAssertEqual(mints.calls, 2)
    }

    func testRetryDoesNothingWhenThereIsNothingToRetry() async {
        let mints = Mints()
        let offer = makeOffer(mints)
        await offer.retry()
        XCTAssertEqual(offer.state, .idle)
        XCTAssertEqual(mints.calls, 0)
    }
}

// MARK: - The link a person reads before they send it

@MainActor
final class CallInviteShortLinkTests: XCTestCase {

    /// A person about to send a link should see WHICH link. The panel that
    /// showed only a Copy button beside the words "Call link" asked them to
    /// send something they had never seen.
    func testTheSchemeComesOffBecauseItIsTheSameOnEveryLink() {
        XCTAssertEqual(
            CallInviteOfferRules.shortLink("https://lakko.me/c/abc"),
            "lakko.me/c/abc")
        XCTAssertEqual(
            CallInviteOfferRules.shortLink("http://lakko.me/c/abc"),
            "lakko.me/c/abc")
    }

    /// Shortening here as well as in the layout elided one link twice, which
    /// reads as a broken string rather than a shortened one. How much fits is
    /// a question about a particular screen; this only drops what is noise on
    /// every screen.
    func testItDoesNotShortenBecauseTheLayoutAlreadyDoes() {
        let long = "https://lakko.me/c/" + String(repeating: "A", count: 120)
        let short = CallInviteOfferRules.shortLink(long)
        XCTAssertFalse(short.contains("\u{2026}"), short)
        XCTAssertEqual(short, String(long.dropFirst("https://".count)))
    }

    func testAShortLinkIsLeftAloneApartFromItsScheme() {
        XCTAssertEqual(
            CallInviteOfferRules.shortLink("https://lakko.me/c/AbC123"),
            "lakko.me/c/AbC123")
    }
}
