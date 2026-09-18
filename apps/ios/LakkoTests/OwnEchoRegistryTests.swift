// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// The own-echo filter, and the live defect it caused: a row this device
/// published in one chat silently swallowed another chat's row of the same
/// number, permanently.
///
/// ## What was actually dropping the row
///
/// ``ChatClient`` kept the rows this device published in a bare `Set<Int64>` of
/// DS message ids, with no group in the key, and `applyFetchedMessages`
/// consulted it for every row of every group. A DS id is assigned **per
/// journal**, and there is one journal per group, so id 7 exists independently
/// in every chat. Publishing anything in chat A therefore armed a trap for the
/// next row numbered the same in chat B: the poll dropped it as "our own echo"
/// and advanced the cursor past it — the cursor moves BEFORE the filter runs —
/// so it was gone for good.
///
/// Everything rides that journal, so everything was in range: a message, an
/// edit, a reaction, a read receipt, a service envelope, a membership commit,
/// and an incoming call's offer.
///
/// ## Why it looked like a screen dependence
///
/// It is a function of WHO applies the rows. A chat that is open has its own
/// uninterrupted long poll and consumes its own echoes within a second. Every
/// other chat is applied by the app-level sweep
/// (``ForegroundReceiveScheduler``), where every group's rows run through the
/// one shared filter — so an offer for a chat the user was not looking at was
/// swallowed by an id left behind by a message they had sent somewhere else.
/// The call was fetched and dropped, not fetched late.
///
/// These tests drive the ALL-GROUPS shape — several groups sharing one
/// ``ChatClient`` — rather than the single-group chat-screen shape, which is
/// exactly the difference that hid the bug.
///
/// ## Why the existing coverage did not catch it
///
/// ``CallHostTests`` already pins that an offer on a chat with no screen open
/// rings, and it passed throughout: it enters at
/// `ChatRepository.routeCallSignaling`, which is BELOW the own-echo filter — by
/// the time a row reaches it the drop has already happened.
/// ``ReceiveDurabilityTests`` drives `applyFetchedMessages` itself, but with one
/// group, where a per-journal key and an id-only key agree on every row.
///
/// ## Measured discrimination — the id-only key was RESTORED and the suite RUN
///
/// With ``OwnEchoRegistry``'s key reverted to the id alone (the group argument
/// accepted and ignored), 5 of the 8 tests below failed, 13 assertions, quoted
/// verbatim:
///
///     testAnIdPublishedInOneChatNeverSilencesAnotherChatsRow : XCTAssertFalse
///     failed - group B's row 7 is not our echo
///     …and: XCTAssertTrue failed - group A's row 7 still is
///
///     testTheAllGroupsPollShapeDoesNotConsumeItsWayThroughAChat :
///     XCTAssertTrue failed - bbbb…: its own row 3
///     …and the same for cccc…
///
///     testTwoRealGroupsSharingOneClientDoNotCrossSuppress : XCTAssertTrue
///     failed - group B's row 7 must reach the core: it is not our echo
///     …and: XCTAssertFalse failed - our own echo never reaches the core
///
///     testAnOfferOnAChatWithNoScreenOpenSurvivesTheFilterAndRings :
///     XCTAssertFalse failed - the offer must not be taken for our own echo
///     …and: ("nil") is not equal to
///     ("Optional(Lakko.CallSession.WebCallState.ringing)") - the phone rings
///
///     testForgettingADeadJournalDropsOnlyItsOwnEntries : XCTAssertEqual
///     failed: ("2") is not equal to ("1") - only the dead journal's entries go
///     …and: XCTAssertTrue failed - a live journal's registration survives a
///     sibling's teardown
///
/// The last one is an artifact of the probe rather than a property the id-only
/// key ever had: with no group in the key there is nothing for
/// ``OwnEchoRegistry/forget(groupIdHex:)`` to filter on, so it removes either
/// everything or nothing. It is recorded as it ran rather than trimmed to the
/// four that speak to the collision directly.
///
/// The other three stay green under the probe, and that is the point of them:
/// suppressing our OWN echo, consuming it exactly once, and the id-only
/// counterfactual are behaviour the fix must not change.
///
/// ``ReceiveDurabilityTests`` — 8 tests, the other suite that drives
/// `applyFetchedMessages` — was run against the probe too and stayed entirely
/// green, which is the coverage gap stated as a measurement: with one group, a
/// per-journal key and an id-only key agree on every row.
@MainActor
final class OwnEchoRegistryTests: XCTestCase {

    private let groupA = String(repeating: "aa", count: 16)
    private let groupB = String(repeating: "bb", count: 16)
    private let groupC = String(repeating: "cc", count: 16)

    // MARK: - The key itself

    func testAnIdPublishedInOneChatNeverSilencesAnotherChatsRow() {
        // THE defect, in one assertion. Every journal starts at 1 and the ids
        // stay small, so across a handful of chats this collision is the common
        // case rather than a corner one.
        let echoes = OwnEchoRegistry()
        echoes.remember(groupIdHex: groupA, messageId: 7)

        XCTAssertFalse(
            echoes.consume(groupIdHex: groupB, messageId: 7),
            "group B's row 7 is not our echo"
        )
        XCTAssertFalse(
            echoes.consume(groupIdHex: groupC, messageId: 7),
            "nor is group C's"
        )
        XCTAssertTrue(
            echoes.consume(groupIdHex: groupA, messageId: 7),
            "group A's row 7 still is"
        )
    }

    func testOurOwnEchoIsStillSuppressedOnce() {
        let echoes = OwnEchoRegistry()
        echoes.remember(groupIdHex: groupA, messageId: 42)

        XCTAssertTrue(echoes.consume(groupIdHex: groupA, messageId: 42))
        // A published row comes back exactly once; a second match would mean a
        // later, unrelated row of the same number.
        XCTAssertFalse(echoes.consume(groupIdHex: groupA, messageId: 42))
        XCTAssertEqual(echoes.count, 0)
    }

    func testTheAllGroupsPollShapeDoesNotConsumeItsWayThroughAChat() {
        // The app-level sweep applies every group through one registry. Register
        // a send in each of three chats, then walk the ids every chat carries.
        let echoes = OwnEchoRegistry()
        let chats = [groupA, groupB, groupC]
        for chat in chats { echoes.remember(groupIdHex: chat, messageId: 3) }

        // Each chat suppresses only its own row 3 ...
        for chat in chats {
            XCTAssertTrue(
                echoes.consume(groupIdHex: chat, messageId: 3),
                "\(chat): its own row 3"
            )
        }
        XCTAssertEqual(echoes.count, 0)
        // ... and nothing is left to swallow anyone's next rows.
        for chat in chats {
            for id in Int64(1)...Int64(10) {
                XCTAssertFalse(
                    echoes.consume(groupIdHex: chat, messageId: id),
                    "\(chat) row \(id)"
                )
            }
        }
    }

    func testDuplicateRegistrationIsHarmlessAndClearStartsOver() {
        let echoes = OwnEchoRegistry()
        echoes.remember(groupIdHex: groupA, messageId: 1)
        echoes.remember(groupIdHex: groupA, messageId: 1)
        XCTAssertEqual(echoes.count, 1)

        echoes.remember(groupIdHex: groupB, messageId: 2)
        echoes.clear()
        XCTAssertFalse(echoes.consume(groupIdHex: groupA, messageId: 1))
        XCTAssertFalse(echoes.consume(groupIdHex: groupB, messageId: 2))
    }

    /// The never-consumed class: a row registered for a journal this device
    /// then stops polling. Under the id-only key such an entry was a permanent
    /// trap for that number in every OTHER chat; under the pair it is only dead
    /// weight, but it is still dropped at the two moments that make it dead.
    func testForgettingADeadJournalDropsOnlyItsOwnEntries() {
        let echoes = OwnEchoRegistry()
        echoes.remember(groupIdHex: groupA, messageId: 4)
        echoes.remember(groupIdHex: groupA, messageId: 5)
        echoes.remember(groupIdHex: groupB, messageId: 4)

        echoes.forget(groupIdHex: groupA)

        XCTAssertEqual(echoes.count, 1, "only the dead journal's entries go")
        XCTAssertFalse(echoes.consume(groupIdHex: groupA, messageId: 4))
        XCTAssertFalse(echoes.consume(groupIdHex: groupA, messageId: 5))
        XCTAssertTrue(
            echoes.consume(groupIdHex: groupB, messageId: 4),
            "a live journal's registration survives a sibling's teardown"
        )
    }

    // MARK: - Through the real receive path, two real groups, one client

    /// The production shape, with real FFI `Group`s: one ``ChatClient`` applies
    /// batches from two journals, exactly as the app-level sweep does.
    ///
    /// The junk payload is deliberate — what is under test is whether the row
    /// REACHES the core at all, and `touchedMlsState` is set before the core is
    /// called precisely because a row that then throws may still have
    /// ratcheted. A suppressed row never gets that far, so the flag is the
    /// signal for "the filter let it through".
    func testTwoRealGroupsSharingOneClientDoNotCrossSuppress() throws {
        let identity = try Identity.fromSeed(seed: Data(repeating: 0x71, count: 32))
        let a = try Group.create(identity: identity)
        let b = try Group.create(identity: identity)
        let journalA = DsClient.pseudonymHex(try a.groupId())
        let journalB = DsClient.pseudonymHex(try b.groupId())
        XCTAssertNotEqual(journalA, journalB, "two groups, two journals")

        let client = ChatClient()
        // The user sent something in chat A. Its id is 7 on THAT journal.
        client.markOwnPublishedCommit(groupIdHex: journalA, messageId: 7)

        // The sweep now applies chat B, whose journal also reaches row 7.
        let inB = client.applyFetchedMessages(
            identity: identity,
            group: b,
            groupIdHex: journalB,
            raw: fetchResult([(7, Data([0xFF, 0xFF, 0xFF]))]),
            after: 0
        )
        XCTAssertTrue(
            inB.touchedMlsState,
            "group B's row 7 must reach the core: it is not our echo"
        )
        XCTAssertEqual(inB.cursor, 7)

        // And chat A's own row 7 is still suppressed — applying B consumed
        // nothing of A's.
        let inA = client.applyFetchedMessages(
            identity: identity,
            group: a,
            groupIdHex: journalA,
            raw: fetchResult([(7, Data([0xFF, 0xFF, 0xFF]))]),
            after: 0
        )
        XCTAssertFalse(
            inA.touchedMlsState,
            "our own echo never reaches the core"
        )
        XCTAssertEqual(
            inA.cursor, 7,
            "and a suppressed row still counts for the cursor — it was delivered"
        )
        XCTAssertEqual(client.ownPublishedCount, 0)
    }

    // MARK: - The whole chain: filter, then the call host rings

    /// The live symptom end to end: an offer arrives on a chat with no screen
    /// open, survives the own-echo filter, and reaches the app-level call host,
    /// which rings.
    ///
    /// The filter is driven directly rather than through `applyFetchedMessages`
    /// because decrypting a real offer needs a second party in the group; what
    /// this adds over the cases above is that the SURVIVING row is then handed
    /// to the real ``CallHost`` through the real repository funnel and really
    /// does ring.
    func testAnOfferOnAChatWithNoScreenOpenSurvivesTheFilterAndRings() throws {
        let echoes = OwnEchoRegistry()
        // The user sent a message in the chat they had open. Its id is 7 on
        // THAT journal.
        echoes.remember(groupIdHex: groupA, messageId: 7)

        let host = makeHost()
        let repo = ChatRepository()
        host.attach(to: repo)

        // The all-groups sweep now applies group B, whose journal also reaches
        // row 7 — and that row is the incoming call.
        let offerRowId: Int64 = 7
        let suppressed = echoes.consume(groupIdHex: groupB, messageId: offerRowId)
        XCTAssertFalse(suppressed, "the offer must not be taken for our own echo")

        // Surviving the filter is what lets it reach the host at all.
        if !suppressed {
            let bytes = try composeCallSignaling(
                body: .offer(
                    callId: Data(repeating: 0x11, count: 16),
                    sdp: "v=0",
                    createdAtMs: 1,
                    media: FfiCallMediaFlags(audio: true, video: false)
                )
            )
            repo.routeCallSignaling(
                groupIdHex: groupB,
                messageId: offerRowId,
                payload: bytes,
                createdAt: dsTime(nowMs - 2_000),
                senderLeaf: 1,
                senderDeviceKey: "device-1",
                senderKey: "member-1"
            )
        }

        XCTAssertEqual(host.activeCall?.state, .ringing, "the phone rings")
        XCTAssertEqual(host.activeGroupIdHex, groupB)
    }

    /// The regression this closes, stated as the behaviour it replaces: with
    /// ids alone, group A's row 7 matches group B's row 7 and the call is
    /// dropped before anything can ring for it.
    func testTheOldIdOnlyFilterWouldHaveSwallowedThatSameOffer() {
        var idsOnly: Set<Int64> = [7]
        XCTAssertNotNil(
            idsOnly.remove(7),
            "the old filter ate another chat's row"
        )
    }

    // MARK: - Fixtures

    private let nowMs: Int64 = 1_800_000_000_000

    private func fetchResult(_ rows: [(Int64, Data)]) -> FetchResult {
        FetchResult(
            messages: rows.map {
                MailboxMessage(id: $0.0, payload: $0.1, createdAt: "t-\($0.0)")
            },
            hasMore: false
        )
    }

    private func dsTime(_ ms: Int64) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: Date(timeIntervalSince1970: TimeInterval(ms) / 1000))
    }

    private final class NoopTimer: CallHostTimer {
        func cancel() {}
    }

    /// A host with no WebRTC anywhere: nothing here accepts or dials, so no
    /// peer connection is ever built.
    private func makeHost() -> CallHost {
        CallHost(ports: CallHost.Ports(
            publish: { _, _ in true },
            deriveCallKey: { _, _ in (Data(repeating: 0x42, count: 32), 0) },
            nowMs: { [nowMs] in nowMs },
            schedule: { _, _ in NoopTimer() },
            microphoneGranted: { true },
            cameraGranted: { _ in true },
            peerName: { _ in "Alice" },
            mintCallId: { Data(repeating: 0x0C, count: 16) },
            recordCall: { _, _ in }
        ))
    }
}
