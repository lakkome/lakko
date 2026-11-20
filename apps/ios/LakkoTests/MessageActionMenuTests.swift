// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI
import UIKit
import XCTest

@testable import Lakko

/// `docs/ux/message-actions.md` sections 2 and 3 — what the per-message menu
/// contains and what it looks like.
///
/// Three kinds of assertion, and which kind a rule gets is not arbitrary:
///
///   * the ACTION SET is a pure function, so it is asserted directly;
///   * the SHAPE (a row that is a row, a card that sizes itself) is real SwiftUI
///     layout, measured with `UIHostingController.sizeThatFits(in:)` the way
///     `MediaBubbleLayoutTests` measures the voice bubble — the production views
///     are laid out without ever appearing;
///   * the GESTURE is not here at all. A rule about how long a finger is held
///     needs a finger; `LakkoUITests/MessageActionMenuUITests` holds one.
@MainActor
final class MessageActionMenuTests: XCTestCase {

    /// Wide and tall enough that nothing is squeezed, finite so a height-greedy
    /// view reports an obviously wrong number instead of an unreadable one.
    private let roomy = CGSize(width: 400, height: 4_000)

    // MARK: - Section 3: which actions

    /// The case that was wrong before this: on someone else's message the whole
    /// menu was one row reading `React >`.
    ///
    /// What it must be now is Copy and nothing else — and in particular NOT
    /// Delete, which is the assertion that matters: Delete on another person's
    /// message is an action the author-only receive gate (ADR-0023) would refuse
    /// anyway, so offering it would be a control that silently does nothing.
    func testSomeoneElsesMessageOffersCopyAndNeitherEditNorDelete() {
        let actions = MessageMenuAction.offered(canReply: false, canForward: false, canEditOrDelete: false, hasText: true)

        XCTAssertEqual(actions, [.copy])
        XCTAssertFalse(actions.contains(.delete), "Delete was offered on someone else's message")
        XCTAssertFalse(actions.contains(.edit), "Edit was offered on someone else's message")
    }

    /// Own, not deleted — and in section 2's order: the one that only reads, then
    /// the one that changes the message, then the destructive one last.
    func testOwnMessageOffersCopyEditAndDeleteInThatOrder() {
        XCTAssertEqual(
            MessageMenuAction.offered(canReply: false, canForward: false, canEditOrDelete: true, hasText: true),
            [.copy, .edit, .delete]
        )
    }

    /// ADR-0052 / section 2 — a new-message action comes before everything
    /// else: Reply is the first row, on own and on other people's messages
    /// alike, and Delete is still last.
    func testReplyIsOfferedFirstOnAnyMessageWithAnId() {
        XCTAssertEqual(
            MessageMenuAction.offered(canReply: true, canForward: false, canEditOrDelete: true, hasText: true),
            [.reply, .copy, .edit, .delete]
        )
        XCTAssertEqual(
            MessageMenuAction.offered(canReply: true, canForward: false, canEditOrDelete: false, hasText: true),
            [.reply, .copy]
        )
        XCTAssertEqual(
            MessageMenuAction.offered(canReply: true, canForward: false, canEditOrDelete: false, hasText: false),
            [.reply]
        )
    }

    /// ADR-0052 decision 2 / section 2 — Forward is the SECOND row, right
    /// after Reply and before everything that only reads or changes: Reply,
    /// Forward, Copy, Edit, Delete. On someone else's message the same two
    /// lead and Copy follows.
    func testForwardIsOfferedSecondAfterReply() {
        XCTAssertEqual(
            MessageMenuAction.offered(
                canReply: true, canForward: true, canEditOrDelete: true, hasText: true),
            [.reply, .forward, .copy, .edit, .delete]
        )
        XCTAssertEqual(
            MessageMenuAction.offered(
                canReply: true, canForward: true, canEditOrDelete: false, hasText: true),
            [.reply, .forward, .copy]
        )
        XCTAssertEqual(
            MessageMenuAction.offered(
                canReply: true, canForward: true, canEditOrDelete: false, hasText: false),
            [.reply, .forward]
        )
    }

    /// Section 5.1 — a message still sending or one whose send failed does not
    /// offer Forward even though it offers Reply (the caller passes the two
    /// separately); Forward alone is a valid card too.
    func testForwardIsWithheldIndependentlyOfReply() {
        XCTAssertEqual(
            MessageMenuAction.offered(
                canReply: true, canForward: false, canEditOrDelete: false, hasText: false),
            [.reply]
        )
        XCTAssertEqual(
            MessageMenuAction.offered(
                canReply: false, canForward: true, canEditOrDelete: false, hasText: false),
            [.forward]
        )
    }

    /// Section 8 / section 2 — Select is the second read-only row, right after
    /// Copy and before anything that changes the message: Reply, Forward,
    /// Copy, Select, Pin, Edit, Delete. Withheld where the mode could not
    /// select the row (a tombstone, a send not complete), independently of
    /// Copy: a photo with no caption offers Select and no Copy.
    func testSelectSitsAfterCopyAndBeforePin() {
        XCTAssertEqual(
            MessageMenuAction.offered(
                canReply: true, canForward: true, canEditOrDelete: true, hasText: true, pin: .pin,
                canSelect: true),
            [.reply, .forward, .copy, .select, .pin, .edit, .delete])
        XCTAssertEqual(
            MessageMenuAction.offered(
                canReply: true, canForward: true, canEditOrDelete: false, hasText: false, canSelect: true),
            [.reply, .forward, .select])
        XCTAssertEqual(
            MessageMenuAction.offered(
                canReply: true, canForward: false, canEditOrDelete: false, hasText: true, canSelect: false),
            [.reply, .copy])
        XCTAssertEqual(MessageMenuAction.select.title, "Select")
        XCTAssertFalse(MessageMenuAction.select.isDestructive)
    }

    /// Forward has its own title and its own glyph, the mirror of Reply's.
    func testForwardHasItsOwnTitleAndGlyph() {
        XCTAssertEqual(MessageMenuAction.forward.title, "Forward")
        XCTAssertEqual(MessageMenuAction.forward.icon, .forward)
        XCTAssertFalse(MessageMenuAction.forward.isDestructive)
    }

    /// A tombstone and a row that has not landed offer no Reply (the caller
    /// passes `canReply: false`); with nothing else the card is empty.
    func testNoReplyMeansNoFirstRow() {
        XCTAssertEqual(
            MessageMenuAction.offered(canReply: false, canForward: false, canEditOrDelete: false, hasText: false),
            []
        )
    }

    /// Section 3 — Copy is offered on "any message with text". A voice message,
    /// a photo with no caption and a tombstone have none.
    func testCopyIsOfferedOnlyWhereThereIsText() {
        XCTAssertEqual(
            MessageMenuAction.offered(canReply: false, canForward: false, canEditOrDelete: true, hasText: false),
            [.edit, .delete]
        )
        XCTAssertEqual(
            MessageMenuAction.offered(canReply: false, canForward: false, canEditOrDelete: false, hasText: false),
            [],
            "a message with nothing to copy and nothing to change offered an action anyway"
        )
    }

    /// Exactly one action is destructive, and it is the last one.
    ///
    /// Pinned because the card draws its separator from `isDestructive` and
    /// `MessageMenuAction.offered` puts it last by construction; a second
    /// destructive action added in the middle would quietly produce a separator
    /// in the wrong place.
    func testDeleteIsTheOnlyDestructiveActionAndComesLast() {
        // ADR-0099 — Report joins the destructive group; `offered` puts it
        // last and only when Delete is not offered (never on an own message).
        XCTAssertEqual(MessageMenuAction.allCases.filter(\.isDestructive), [.delete, .ban, .report])
        XCTAssertEqual(MessageMenuAction.offered(canReply: false, canForward: false, canEditOrDelete: true, hasText: true).last, .delete)
    }

    // MARK: - Section 2: the shape

    /// The reaction row is a ROW.
    ///
    /// This is the defect the whole reshape is for: the same six emoji used to be
    /// a `Menu`, which SwiftUI draws as a vertical list. A column of six 36 pt
    /// targets measures about 216 pt tall and 36 wide; the row measures about 288
    /// wide and 42 tall. Comparing the two dimensions therefore discriminates
    /// between the shape that shipped and the shape the spec asks for, which a
    /// count of buttons would not.
    func testTheReactionRowIsWiderThanItIsTall() {
        let size = measure(MessageReactionRow(onReact: { _ in }, onMore: {}))

        XCTAssertGreaterThan(
            size.width,
            size.height * 3,
            "the reaction row measured \(size) — that is a column, not a row"
        )
        // Seven targets (six emoji and the picker control) have to be in there.
        XCTAssertGreaterThan(
            size.width,
            MessageReactionRow.target * 6,
            "the row is too narrow to hold its own tap targets: \(size)"
        )
        XCTAssertGreaterThan(size.height, 0, "the row measured nothing at all")
    }

    /// Section 2 — "the action card is sized by its own content … and must not
    /// inherit its width."
    ///
    /// Measured as an invariance rather than as a number, because the number
    /// would be a restatement of the padding. What is asserted is that the card
    /// measures the SAME under two very different proposals: a card that took the
    /// width it was offered would report 400 for one and 1 000 for the other, and
    /// in the menu's column that is exactly the failure mode — the column is as
    /// wide as the conversation, so a stretchy card ends up the width of the
    /// screen and the rows it holds end up stretched to the reaction row's width,
    /// which is what section 2 records web having done.
    func testTheActionCardDoesNotTakeTheWidthItIsOffered() {
        let card = MessageActionCard(actions: [.copy, .edit, .delete], perform: { _ in })

        let narrow = measure(card, in: CGSize(width: 400, height: 4_000))
        let wide = measure(card, in: CGSize(width: 1_000, height: 4_000))

        XCTAssertEqual(
            narrow.width,
            wide.width,
            accuracy: 0.5,
            "the card grew with the space it was offered: \(narrow.width) then \(wide.width)"
        )
        XCTAssertLessThan(
            narrow.width,
            300,
            "the card measured \(narrow.width) pt wide for three short words, so it is filling its container"
        )
        XCTAssertGreaterThan(narrow.height, 0, "the card measured nothing at all")
    }

    /// The two surfaces are independent sizes, and the card is the smaller one.
    ///
    /// The companion to the test above from the other side: that one proves the
    /// card ignores its proposal, and this one proves the consequence the spec
    /// cares about — put beside the reaction row, the card is visibly its own
    /// width rather than the row's.
    func testTheCardIsNarrowerThanTheReactionRow() {
        let row = measure(MessageReactionRow(onReact: { _ in }, onMore: {}))
        let card = measure(MessageActionCard(actions: [.copy, .edit, .delete], perform: { _ in }))

        XCTAssertLessThan(
            card.width,
            row.width,
            "the card (\(card.width)) is as wide as the reaction row (\(row.width))"
        )
    }

    /// A card with one action is narrower than nothing else in the menu and
    /// taller than nothing — but it must still be a card, not a sliver.
    func testACardWithOneActionStillMeasuresSomething() {
        let one = measure(MessageActionCard(actions: [.copy], perform: { _ in }))
        let three = measure(MessageActionCard(actions: [.copy, .edit, .delete], perform: { _ in }))

        XCTAssertGreaterThan(one.height, 0)
        XCTAssertLessThan(
            one.height,
            three.height,
            "a one-action card is as tall as a three-action one, so the rows are not stacking"
        )
    }

    // MARK: - Section 2: where the stack goes

    /// The common case: room above for the row and below for the card, so the
    /// bubble does not move at all. Section 2 — "the message being acted on stays
    /// fully legible in place".
    func testABubbleWithRoomAroundItDoesNotMove() {
        let resolved = MessageMenuLayout.resolve(
            bubbleTop: 400,
            bubbleHeight: 60,
            rowHeight: 42,
            cardHeight: 140,
            containerHeight: 900,
            safeTop: 50,
            safeBottom: 34
        )

        XCTAssertEqual(resolved.bubbleShift, 0, accuracy: 0.001, "the bubble moved when it did not have to")
        XCTAssertEqual(
            resolved.stackTop,
            400 - 42 - MessageMenuLayout.gap,
            accuracy: 0.001,
            "the reaction row is not sitting one gap above the bubble"
        )
    }

    /// A bubble near the bottom: the card does not fit under it, so the whole
    /// stack comes up — and the amount it comes up is the least that brings the
    /// card inside the safe area, not a fixed nudge.
    func testABubbleNearTheBottomLiftsTheWholeStackJustEnough() {
        let container: CGFloat = 900
        let safeBottom: CGFloat = 34
        let resolved = MessageMenuLayout.resolve(
            bubbleTop: 800,
            bubbleHeight: 60,
            rowHeight: 42,
            cardHeight: 140,
            containerHeight: container,
            safeTop: 50,
            safeBottom: safeBottom
        )

        XCTAssertLessThan(resolved.bubbleShift, 0, "the stack did not move up for a bubble at the bottom")
        let total = 42 + MessageMenuLayout.gap + 60 + MessageMenuLayout.gap + 140
        XCTAssertEqual(
            resolved.stackTop + total,
            container - safeBottom - MessageMenuLayout.margin,
            accuracy: 0.001,
            "the card's bottom edge is not resting on the margin, so the lift was not the smallest one"
        )
    }

    /// A bubble at the very top: the reaction row would sit off screen, so the
    /// stack comes down to the margin and the bubble follows it.
    func testABubbleAtTheTopPushesTheStackDownToTheMargin() {
        let resolved = MessageMenuLayout.resolve(
            bubbleTop: 60,
            bubbleHeight: 60,
            rowHeight: 42,
            cardHeight: 140,
            containerHeight: 900,
            safeTop: 50,
            safeBottom: 34
        )

        XCTAssertEqual(
            resolved.stackTop,
            50 + MessageMenuLayout.margin,
            accuracy: 0.001,
            "the reaction row is not inside the safe area"
        )
        XCTAssertGreaterThan(resolved.bubbleShift, 0, "the bubble did not follow the row down")
    }

    /// A stack taller than the screen it is on. The row and the bubble stay
    /// visible and the card runs off the bottom, because the bubble is what says
    /// which message is being acted on.
    func testAStackTallerThanTheScreenKeepsTheRowAndTheBubble() {
        let resolved = MessageMenuLayout.resolve(
            bubbleTop: 200,
            bubbleHeight: 700,
            rowHeight: 42,
            cardHeight: 140,
            containerHeight: 500,
            safeTop: 50,
            safeBottom: 34
        )

        XCTAssertEqual(
            resolved.stackTop,
            50 + MessageMenuLayout.margin,
            accuracy: 0.001,
            "a stack that cannot fit was not pinned to the top"
        )
        XCTAssertGreaterThan(
            resolved.stackTop + 42 + MessageMenuLayout.gap,
            0,
            "the bubble was pushed off the top of the screen"
        )
    }

    /// A message that offers no reaction row reserves no space for one, and the
    /// same for the card. Otherwise a tombstone's menu would hover a gap above
    /// the bubble for a row that is not there.
    func testAnAbsentSurfaceReservesNoSpace() {
        let noRow = MessageMenuLayout.resolve(
            bubbleTop: 400,
            bubbleHeight: 60,
            rowHeight: 0,
            cardHeight: 140,
            containerHeight: 900,
            safeTop: 50,
            safeBottom: 34
        )
        XCTAssertEqual(noRow.stackTop, 400, accuracy: 0.001)
        XCTAssertEqual(noRow.bubbleShift, 0, accuracy: 0.001)

        let noCard = MessageMenuLayout.resolve(
            bubbleTop: 820,
            bubbleHeight: 60,
            rowHeight: 42,
            cardHeight: 0,
            containerHeight: 900,
            safeTop: 50,
            safeBottom: 34
        )
        // 820 + 60 = 880, and the bottom margin allows 900 - 34 - 12 = 854, so a
        // card-less menu still lifts — but only by what the bubble itself needs.
        XCTAssertEqual(noCard.stackTop + 42 + MessageMenuLayout.gap + 60, 854, accuracy: 0.001)
    }

    // MARK: - Section 3: Copy

    /// "Puts the text on the clipboard. Nothing leaves the device."
    ///
    /// The pasteboard itself, not a spy: Copy has one job and the only way to
    /// know it did it is to read what a person would paste.
    func testCopyPutsTheMessageTextOnThePasteboard() {
        let text = "the text a person can see " + UUID().uuidString
        MessageClipboard.copy(text)

        XCTAssertEqual(UIPasteboard.general.string, text)
    }

    /// An empty copy is a no-op rather than a clear.
    ///
    /// The card does not offer Copy on a message with no text, so this is the
    /// second line of defence — and the failure it prevents is destructive in a
    /// way the missing row is not: a mis-wired caller would erase whatever the
    /// person had copied somewhere else.
    func testCopyingNothingLeavesThePasteboardAlone() {
        let kept = "kept " + UUID().uuidString
        MessageClipboard.copy(kept)
        MessageClipboard.copy("")

        XCTAssertEqual(UIPasteboard.general.string, kept)
    }

    // MARK: - The gesture threshold

    /// One threshold, two readers.
    ///
    /// The bubble opens the menu at it and the waveform refuses a seek past it
    /// (`client-redesign.md`: a press that opened the menu is not also a seek).
    /// Two copies of the number would drift, and a waveform gate at 0.5 s against
    /// a menu that opened at 0.4 s means one press does both things — which is
    /// the outcome the rule exists to forbid, reached through a constant rather
    /// than through a gesture.
    func testTheSeekGateAndTheMenuAgreeOnWhatALongPressIs() {
        XCTAssertEqual(VoiceSeekGate.longPressSeconds, MessageLongPress.thresholdSeconds)
        XCTAssertFalse(
            VoiceSeekGate.acceptsSeek(pressSeconds: MessageLongPress.thresholdSeconds),
            "a press exactly long enough to open the menu was also accepted as a seek"
        )
        XCTAssertTrue(VoiceSeekGate.acceptsSeek(pressSeconds: MessageLongPress.thresholdSeconds - 0.05))
    }

    // MARK: - Harness

    // MARK: - The design-system sizes (reaction chip, quick reaction bar)

    /// Design system "Reaction chip": emoji 28 in a 44 square, 4 apart. Seven
    /// targets, six gaps and the panel's inset on each side.
    func testTheReactionRowIsDrawnAtTheDesignSystemSize() {
        let size = measure(
            MessageReactionRow(onReact: { _ in }, onMore: {}).environment(\.dynamicTypeSize, .large))
        let expected = 7 * MessageReactionRow.target + 6 * MessageReactionRow.gap
            + 2 * MessageReactionRow.inset
        XCTAssertEqual(MessageReactionRow.target, 44)
        XCTAssertEqual(MessageReactionRow.emojiSize, 28)
        XCTAssertEqual(MessageReactionRow.gap, 4)
        XCTAssertEqual(size.width, expected, accuracy: 0.5, "the row measured \(size)")
        XCTAssertGreaterThanOrEqual(size.height, MessageReactionRow.target)
    }

    /// `docs/ux/text-size.md` — where the row is wider than the screen less
    /// its margins (a narrow phone, a large text size) it scrolls sideways:
    /// the panel is the width it is given and the targets keep at least 44.
    func testTheReactionRowScrollsWhenItDoesNotFit() {
        for (width, size) in [(CGFloat(320), DynamicTypeSize.large), (375, .accessibility5)] {
            let row = MessageReactionRow(maxWidth: width - 16, onReact: { _ in }, onMore: {})
                .environment(\.dynamicTypeSize, size)
            let measured = measure(row)
            XCTAssertEqual(measured.width, width - 16, accuracy: 0.5, "the row measured \(measured) at \(width)")
            XCTAssertGreaterThanOrEqual(measured.height, MessageReactionRow.target)
        }
    }

    /// A row that fits does not scroll: it keeps its own width.
    func testTheReactionRowThatFitsKeepsItsOwnWidth() {
        let row = MessageReactionRow(maxWidth: 375 - 16, onReact: { _ in }, onMore: {})
            .environment(\.dynamicTypeSize, .large)
        let expected = 7 * MessageReactionRow.target + 6 * MessageReactionRow.gap + 2 * MessageReactionRow.inset
        XCTAssertEqual(measure(row).width, expected, accuracy: 0.5)
    }

    /// The row hung off an incoming bubble near the far side moves back inside
    /// the screen; one that fits does not move.
    func testTheReactionRowMovesBackInsideTheScreen() {
        let width: CGFloat = 375
        let row: CGFloat = 344
        let near = CGRect(x: 52, y: 0, width: 120, height: 40)
        let shift = MessageMenuLayout.rowShift(
            rowWidth: row, bubble: near, containerWidth: width, outgoing: false)
        XCTAssertEqual(near.minX + shift + row, width - MessageMenuLayout.sideMargin, accuracy: 0.01)
        XCTAssertEqual(
            MessageMenuLayout.rowShift(
                rowWidth: row, bubble: CGRect(x: 8, y: 0, width: 120, height: 40),
                containerWidth: width, outgoing: false),
            0)
        let outgoing = CGRect(x: 250, y: 0, width: 117, height: 40)
        XCTAssertEqual(
            MessageMenuLayout.rowShift(
                rowWidth: row, bubble: outgoing, containerWidth: width, outgoing: true),
            0, "an outgoing bubble at the trailing side has room for the row on its left")
        let shortOutgoing = CGRect(x: 200, y: 0, width: 100, height: 40)
        let right = MessageMenuLayout.rowShift(
            rowWidth: row, bubble: shortOutgoing, containerWidth: width, outgoing: true)
        XCTAssertEqual(shortOutgoing.maxX + right - row, MessageMenuLayout.sideMargin, accuracy: 0.01)
    }

    /// Design system "Reaction chip": 32 tall, at the default text size, for
    /// every face; the "+N" chip the same height.
    func testAReactionChipIsThirtyTwoTall() {
        XCTAssertEqual(ReactionChipMetrics.height, 32)
        XCTAssertEqual(ReactionChipMetrics.chipSpacing, 6)
        let faces: [ReactionChipLabel] = [
            ReactionChipLabel(emoji: "👍", content: .emojiOnly),
            ReactionChipLabel(emoji: "👍", content: .count("12")),
            ReactionChipLabel(
                emoji: "👍",
                content: .avatars([
                    (name: "Alice", identity: "a", photo: nil),
                    (name: "Bob", identity: "b", photo: nil),
                    (name: "Carol", identity: "c", photo: nil),
                ]),
                mine: true),
            ReactionChipLabel(emoji: nil, content: .count("+3")),
        ]
        for chip in faces {
            let size = measure(chip.environment(\.dynamicTypeSize, .large))
            XCTAssertEqual(size.height, 32, accuracy: 0.5, "a chip measured \(size)")
        }
    }

    /// A larger text size grows the chip: the sizes scale, they are not fixed.
    func testAReactionChipGrowsWithTheTextSize() {
        let chip = ReactionChipLabel(emoji: "👍", content: .count("2"))
        let normal = measure(chip.environment(\.dynamicTypeSize, .large))
        let large = measure(chip.environment(\.dynamicTypeSize, .accessibility3))
        XCTAssertGreaterThan(large.height, normal.height)
        XCTAssertGreaterThan(large.width, normal.width)
    }

    /// Lay a production view out without letting it appear, the technique
    /// `MediaBubbleLayoutTests` established.
    private func measure<V: View>(_ view: V, in proposal: CGSize? = nil) -> CGSize {
        UIHostingController(rootView: view).sizeThatFits(in: proposal ?? roomy)
    }
}

/// `docs/ux/text-size.md` — the per-device chat text size.
final class ChatTextSizeTests: XCTestCase {

    private func freshDefaults() -> UserDefaults {
        let name = "ChatTextSizeTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        defaults.removePersistentDomain(forName: name)
        return defaults
    }

    func testNothingStoredReadsAsMedium() {
        XCTAssertEqual(ChatTextSize.read(freshDefaults()), .medium)
        XCTAssertEqual(ChatTextSize.from(stored: nil), .medium)
    }

    func testAnUnknownStoredValueReadsAsMedium() {
        let defaults = freshDefaults()
        defaults.set("huge", forKey: ChatTextSize.storageKey)
        XCTAssertEqual(ChatTextSize.read(defaults), .medium)
        XCTAssertEqual(ChatTextSize.from(stored: ""), .medium)
        XCTAssertEqual(ChatTextSize.from(stored: "Large"), .medium, "the stored value is the exact raw value")
    }

    func testAStoredStepReadsBack() {
        let defaults = freshDefaults()
        for step in ChatTextSize.allCases {
            ChatTextSize.write(step, defaults)
            XCTAssertEqual(ChatTextSize.read(defaults), step)
        }
    }

    func testTheFactorsAreTheSpecs() {
        XCTAssertEqual(ChatTextSize.small.factor, 0.87)
        XCTAssertEqual(ChatTextSize.medium.factor, 1.0)
        XCTAssertEqual(ChatTextSize.large.factor, 1.2)
        XCTAssertEqual(ChatTextSize.allCases.map(\.labelKey), [
            "settings.text_size.small", "settings.text_size.medium", "settings.text_size.large",
        ])
    }

    /// The chip's sizes move together with the step.
    func testALargeStepGrowsAReactionChip() {
        let chip = ReactionChipLabel(emoji: "👍", content: .count("2"))
        let medium = measure(chip.environment(\.dynamicTypeSize, .large))
        let large = measure(
            chip.environment(\.dynamicTypeSize, .large).environment(\.chatTextScale, ChatTextSize.large.factor))
        XCTAssertEqual(medium.height, 32, accuracy: 0.5)
        XCTAssertEqual(large.height, 32 * 1.2, accuracy: 0.5)
        XCTAssertGreaterThan(large.width, medium.width)
    }

    /// The quick reaction bar's targets never go below 44 at the small step.
    func testASmallStepKeepsTheReactionTargetsAtFortyFour() {
        let row = MessageReactionRow(onReact: { _ in }, onMore: {})
            .environment(\.dynamicTypeSize, .large)
            .environment(\.chatTextScale, ChatTextSize.small.factor)
        let size = measure(row)
        let expected = 7 * MessageReactionRow.target + 6 * MessageReactionRow.gap + 2 * MessageReactionRow.inset
        XCTAssertEqual(size.width, expected, accuracy: 0.5, "the row measured \(size)")
    }

    private func measure<V: View>(_ view: V) -> CGSize {
        UIHostingController(rootView: view).sizeThatFits(in: CGSize(width: 2_000, height: 2_000))
    }
}

/// Base scenario 10 — what a hardware Return does in the composer and the
/// edit sheet.
final class HardwareReturnTests: XCTestCase {
    func testReturnSubmitsAndShiftOrOptionReturnBreaksTheLine() {
        XCTAssertTrue(HardwareReturn.submits([]))
        XCTAssertTrue(HardwareReturn.submits(.command) == true)
        XCTAssertFalse(HardwareReturn.submits(.shift))
        XCTAssertFalse(HardwareReturn.submits(.option))
        XCTAssertFalse(HardwareReturn.submits([.shift, .option]))
    }

    /// Follow-up to base scenario 26: a message drawn as a channel
    /// invitation card offers no Edit (it would put the `/g/` link with its
    /// secret in the composer); Delete stays.
    func testAnInvitationCardOffersDeleteButNeverEdit() {
        let actions = MessageMenuAction.offered(
            canReply: false, canForward: false, canEditOrDelete: true, hasText: false, canEdit: false)
        XCTAssertEqual(actions, [.delete])
    }

    /// Maintainer, 2026-11-13 — no Forward on an invitation card: the screen
    /// hands `canForward: false`, and Pin is still offered.
    func testAnInvitationCardOffersPinButNotForward() {
        let actions = MessageMenuAction.offered(
            canReply: true, canForward: false, canEditOrDelete: true, hasText: false, pin: .pin, canEdit: false)
        XCTAssertFalse(actions.contains(.forward))
        XCTAssertTrue(actions.contains(.pin))
    }
}
