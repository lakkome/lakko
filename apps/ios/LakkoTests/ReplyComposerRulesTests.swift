// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// `docs/ux/message-actions.md` section 3.1, "one thing at a time" — the
/// composer's rules about an armed reply, on the view model that owns them.
///
/// The view model is built over a bare repository (no group, no network), the
/// way `CallHostTests` builds one: every rule here is state that changes on
/// the main actor with nothing sent.
@MainActor
final class ReplyComposerRulesTests: XCTestCase {

    private func model() -> ChatViewModel {
        ChatViewModel(repository: ChatRepository(), groupIdHex: "0102")
    }

    private func incoming(_ id: Int64, _ text: String) -> ChatMessage {
        ChatMessage(
            messageId: id, direction: .incoming, text: text,
            createdAt: "2026-09-26T10:00:00Z", authorKeyB64: "dGhlbQ==")
    }

    private func own(_ id: Int64, _ text: String) -> ChatMessage {
        ChatMessage(
            messageId: id, direction: .outgoing, text: text,
            createdAt: "2026-09-26T10:00:00Z", authorKeyB64: "bWU=")
    }

    func testChoosingReplyArmsTheSnapshotAndAsksForTheComposersFocus() {
        let vm = model()
        let before = vm.composerFocusRequest
        vm.beginReply(incoming(5, "hello\nthere"))
        XCTAssertEqual(vm.replyingTo?.messageId, 5)
        XCTAssertEqual(vm.replyingTo?.snapshot.target, 5)
        XCTAssertEqual(vm.replyingTo?.snapshot.excerpt, "hello there")
        XCTAssertEqual(vm.composerFocusRequest, before + 1)
    }

    /// Maintainer, 2026-11-04: the snapshot on the wire carries the author's
    /// real name, never "You" (other people read it); this device draws "You"
    /// for its own message.
    func testAReplyToAnOwnMessageCarriesTheRealNameAndIsShownAsYouHere() {
        let vm = model()
        vm.beginReply(own(9, "mine"))
        XCTAssertNotEqual(vm.replyingTo?.snapshot.author, "You")
        XCTAssertEqual(vm.replyingTo?.shown.author, "You")
        XCTAssertEqual(vm.replyingTo?.snapshot.excerpt, "mine")
    }

    /// A reply to someone this device has no name for carries no name: never
    /// the "Member" placeholder and never a key fingerprint.
    func testAReplyToAnUnnamedAuthorCarriesNoPlaceholder() {
        let vm = model()
        vm.beginReply(incoming(5, "hello"))
        let author = vm.replyingTo?.snapshot.author
        XCTAssertNil(author, "carried \(author ?? "")")
        XCTAssertNil(vm.forwardAuthorLabel(for: incoming(5, "hello")))
    }

    /// "Choosing Reply while an edit is open ends the edit (the draft is
    /// discarded, as Cancel would)."
    func testReplyEndsAnOpenEditAndDiscardsItsDraft() {
        let vm = model()
        vm.beginEdit(own(9, "mine"))
        vm.editText = "mine, changed"
        vm.beginReply(incoming(5, "hello"))
        XCTAssertNil(vm.editingMessage)
        XCTAssertEqual(vm.editText, "")
        XCTAssertEqual(vm.replyingTo?.messageId, 5)
    }

    /// "Choosing Edit while a reply is armed drops the reply."
    func testEditDropsAnArmedReply() {
        let vm = model()
        vm.beginReply(incoming(5, "hello"))
        vm.beginEdit(own(9, "mine"))
        XCTAssertNil(vm.replyingTo)
        XCTAssertEqual(vm.editingMessage?.messageId, 9)
    }

    /// "Choosing Reply on another message replaces the strip."
    func testReplyOnAnotherMessageReplacesTheStrip() {
        let vm = model()
        vm.beginReply(incoming(5, "first"))
        vm.beginReply(incoming(6, "second"))
        XCTAssertEqual(vm.replyingTo?.messageId, 6)
        XCTAssertEqual(vm.replyingTo?.snapshot.excerpt, "second")
    }

    func testTheCloseControlClearsTheStrip() {
        let vm = model()
        vm.beginReply(incoming(5, "first"))
        vm.cancelReply()
        XCTAssertNil(vm.replyingTo)
    }

    /// A send takes the reply and clears the strip, once: the second send is
    /// plain.
    func testASendTakesTheReplyOnce() {
        let vm = model()
        vm.beginReply(incoming(5, "first"))
        XCTAssertEqual(vm.takeArmedReply()?.target, 5)
        XCTAssertNil(vm.replyingTo)
        XCTAssertNil(vm.takeArmedReply())
    }

    /// The text send path goes through the same take: after Send the strip
    /// is gone even though the send itself fails (no group is open here).
    func testSendClearsTheStrip() {
        let vm = model()
        vm.beginReply(incoming(5, "first"))
        vm.draft = "sure"
        vm.onSendClicked()
        XCTAssertNil(vm.replyingTo)
    }

    /// "Switching conversation clears the strip; a reply is not a draft that
    /// follows the person." Leaving the screen is what switching is.
    func testLeavingTheScreenClearsTheStrip() {
        let vm = model()
        vm.screenVisible = true
        vm.beginReply(incoming(5, "first"))
        vm.screenVisible = false
        XCTAssertNil(vm.replyingTo)
    }

    /// A row that has not landed has no id to point at; the menu does not
    /// offer Reply on it, and the view model refuses it too.
    func testAMessageWithoutAnIdCannotBeRepliedTo() {
        let vm = model()
        vm.beginReply(ChatMessage(
            messageId: 0, direction: .outgoing, text: "sending",
            createdAt: "", ack: .pending, localId: -1))
        XCTAssertNil(vm.replyingTo)
    }
}
