// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import SwiftUI
import UIKit

/// The chat of a group or link call (`docs/ux/calls.md` section 4.1.4;
/// `lakko_calls::call_chat`). iOS mirror of the web `calls/call-chat.ts` and
/// the Android `CallChat.kt`.
///
/// Text messages between the people in the call, for as long as the call
/// lasts. Each message rides the sealed SFU relay, signed with the sender's
/// MLS leaf key, and a receiver shows it only once that peer's announcement
/// verified with the same leaf key. Nothing is kept after the call, nothing is
/// written into the conversation the call came from, nothing is stored on any
/// server. A one-to-one call has no relay and no chat.
///
/// ``CallChatChannel`` is the per-call wire state (this device's sequence
/// numbers, messages held for a peer not yet named, what was already shown);
/// ``CallChatModel`` is what the call screen draws (the messages, the panel,
/// the unread count, the preview). Both are pure: the signing, the verifying
/// and the sending come in through ``CallChatChannel/Deps``, the sending of a
/// message and the naming of a sender through the model's own hooks.
@MainActor
final class CallChatChannel {

    /// A verified message: its sequence number and its text.
    struct Verified: Equatable {
        let seq: UInt64
        let text: String
    }

    /// What the channel is plugged into.
    struct Deps {
        /// This device's signed message as `selfPeerId`, numbered `seq`, or
        /// `nil` when it cannot make one yet (no group).
        var compose: @MainActor (_ selfPeerId: String, _ seq: UInt64, _ text: String) -> Data?
        /// A message from `from`, verified against the leaf key its
        /// announcement verified with, or `nil` on any refusal.
        var verify: @MainActor (_ from: String, _ signerKey: Data, _ bytes: Data) -> Verified?
        /// Seal and send a relay plaintext to the call; `false` when it could
        /// not (no relay key, no socket).
        var sendSealed: @MainActor (_ plaintext: Data) -> Bool
        /// This device's SFU peer id, once the SFU's Welcome named it.
        var selfPeerId: @MainActor () -> String?
        /// A message from `peerId` verified, for the first time.
        var onMessage: @MainActor (_ peerId: String, _ message: Verified) -> Void
    }

    /// At most this many messages are held for one peer whose announcement
    /// has not verified yet; the oldest goes first. Chosen, not measured.
    static let maxPendingPerPeer = 50

    private let deps: Deps
    /// The last sequence number this device used (numbers start at 1).
    private var ownSeq: UInt64 = 0
    /// Own messages that did not go out, by sequence number: the text, and
    /// the signed bytes once they could be made. ``resend(seq:)`` sends the
    /// same bytes again.
    private var unsent: [UInt64: (text: String, wire: Data?)] = [:]
    /// SFU peer id -> the leaf signature key its verified announcement named.
    private var peerKeys: [String: Data] = [:]
    /// SFU peer id -> messages received before its announcement verified, in
    /// arrival order.
    private var pending: [String: [Data]] = [:]
    /// `(peer id, sequence number)` pairs already shown.
    private var shown: Set<String> = []

    init(deps: Deps) {
        self.deps = deps
    }

    /// Send `text` (already checked by ``CallChatText/isSendable(_:)``) as the
    /// next message. Returns its sequence number and whether it went out; a
    /// message that did not is kept for ``resend(seq:)``.
    func send(_ text: String) -> (seq: UInt64, sent: Bool) {
        ownSeq += 1
        let seq = ownSeq
        let wire = deps.selfPeerId().flatMap { deps.compose($0, seq, text) }
        if let wire, deps.sendSealed(wire) {
            return (seq, true)
        }
        unsent[seq] = (text, wire)
        return (seq, false)
    }

    /// Send the message numbered `seq` again: the same signed bytes, or, when
    /// none could be made at the time, the same text under the same number.
    /// Returns whether it went out this time.
    func resend(seq: UInt64) -> Bool {
        guard let held = unsent[seq] else { return false }
        var wire = held.wire
        if wire == nil {
            wire = deps.selfPeerId().flatMap { deps.compose($0, seq, held.text) }
            unsent[seq] = (held.text, wire)
        }
        guard let wire, deps.sendSealed(wire) else { return false }
        unsent.removeValue(forKey: seq)
        return true
    }

    /// `peerId`'s announcement verified with `signerKey`: the messages it
    /// sent before are read now, in the order they arrived.
    func peerIdentified(_ peerId: String, signerKey: Data) {
        guard !signerKey.isEmpty else { return }
        peerKeys[peerId] = signerKey
        guard let held = pending.removeValue(forKey: peerId) else { return }
        for bytes in held { received(from: peerId, bytes: bytes) }
    }

    /// A sealed message from `peerId` opened.
    func received(from peerId: String, bytes: Data) {
        guard let key = peerKeys[peerId] else {
            var held = pending[peerId] ?? []
            held.append(bytes)
            if held.count > Self.maxPendingPerPeer {
                held.removeFirst(held.count - Self.maxPendingPerPeer)
            }
            pending[peerId] = held
            return
        }
        guard let verified = deps.verify(peerId, key, bytes) else {
            NSLog("CallChatChannel: a message did not verify and was dropped")
            return
        }
        // The same message never shows twice (a relay frame re-sent after a
        // reconnect).
        let id = Self.shownKey(peerId, verified.seq)
        guard !shown.contains(id) else { return }
        shown.insert(id)
        deps.onMessage(peerId, verified)
    }

    /// `peerId` left. What it said stays on screen; what it sent before it
    /// was ever named is never shown.
    func peerLeft(_ peerId: String) {
        pending.removeValue(forKey: peerId)
        peerKeys.removeValue(forKey: peerId)
    }

    /// The call ended for this device: everything goes.
    func clear() {
        unsent = [:]
        peerKeys = [:]
        pending = [:]
        shown = []
    }

    /// How many messages are held for `peerId` (tests).
    func pendingCount(_ peerId: String) -> Int { pending[peerId]?.count ?? 0 }

    private static func shownKey(_ peerId: String, _ seq: UInt64) -> String { "\(peerId)#\(seq)" }
}

/// The composer's text rule. The rule itself is the core's
/// (`checkCallChatText`, `clampCallChatText`); this only puts line breaks in
/// one form first.
enum CallChatText {
    /// `\r\n` and a lone `\r` become `\n`.
    static func normaliseLineBreaks(_ text: String) -> String {
        text.replacingOccurrences(of: "\r\n", with: "\n")
            .replacingOccurrences(of: "\r", with: "\n")
    }

    /// What the field holds after an edit: the text cut at the limit, and
    /// whether cutting took something away (the too-long line shows).
    static func edited(_ input: String) -> (text: String, tooLong: Bool) {
        let normalised = normaliseLineBreaks(input)
        let clamped = clampCallChatText(text: normalised)
        return (clamped, clamped != normalised)
    }

    /// Whether `text` can be sent: 1 to 800 bytes, not only white space, no
    /// control character but a line break or a tab.
    static func isSendable(_ text: String) -> Bool {
        checkCallChatText(text: normaliseLineBreaks(text)) == nil
    }
}

/// One message on the call chat's list.
struct CallChatMessage: Identifiable, Equatable {
    enum Sender: Equatable {
        /// This device.
        case own
        /// Another person in the call, by SFU peer id.
        case peer(String)
    }

    enum Delivery: Equatable {
        case sent
        /// Not sent: the bubble is dimmed and offers `Send again`.
        case notSent
    }

    let id: String
    let sender: Sender
    let seq: UInt64
    let text: String
    /// When it arrived on (or was written on) this device.
    let arrivedAt: Date
    var delivery: Delivery

    var isOwn: Bool { sender == .own }
}

/// What the call screen draws of the chat: the messages, the panel, the
/// unread count on the `Chat` circle, the short preview above the bar.
@MainActor
final class CallChatModel: ObservableObject {

    /// The one-line preview above the control bar while the panel is closed.
    struct Preview: Equatable {
        let id: Int
        let peerId: String
        let text: String
    }

    /// How long a preview stays (`docs/ux/calls.md` section 4.1.4).
    static let previewSeconds: TimeInterval = 4

    /// The call can carry a chat: a group or link call whose adapter wired
    /// the send.
    @Published private(set) var available = false
    @Published private(set) var messages: [CallChatMessage] = []
    @Published private(set) var panelOpen = false
    /// Messages from others since the panel was last open.
    @Published private(set) var unread = 0
    @Published private(set) var preview: Preview?
    /// A message arrived while the list was scrolled up: the `New messages`
    /// pill shows.
    @Published private(set) var newBelow = false
    /// Bumped whenever the list should move to its newest message: the panel
    /// opened, this device sent, or a message arrived while the list was at
    /// its newest.
    @Published private(set) var scrollRequest = 0

    /// The list is at its newest message.
    private(set) var atBottom = true

    /// The name a sender's tile shows now (`""` when unnamed), by SFU peer
    /// id; set by the call's state.
    var tileName: (String) -> String? = { _ in nil }
    /// Says a message aloud while the panel is open.
    var announce: (String) -> Void = { text in
        UIAccessibility.post(notification: .announcement, argument: text)
    }
    /// Runs `action` after `seconds` (the preview's expiry); replaced in
    /// tests.
    var schedule: (TimeInterval, @escaping @MainActor () -> Void) -> Void = { seconds, action in
        Task { @MainActor in
            try? await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
            action()
        }
    }
    var now: () -> Date = { Date() }

    private var sendAction: ((String) -> (seq: UInt64, sent: Bool))?
    private var resendAction: ((UInt64) -> Bool)?
    /// The last name each sender's tile showed, for a sender whose tile has
    /// gone (they left).
    private var lastNames: [String: String] = [:]
    private var previewCounter = 0

    /// The call's adapter: messages go out through `send` and again through
    /// `resend`.
    func setActions(
        send: @escaping (String) -> (seq: UInt64, sent: Bool),
        resend: @escaping (UInt64) -> Bool
    ) {
        sendAction = send
        resendAction = resend
        available = true
    }

    // MARK: Sending

    /// Send `text`. Returns whether the composer should empty: `false` when
    /// the text is not sendable or the call carries no chat.
    @discardableResult
    func send(_ text: String) -> Bool {
        let normalised = CallChatText.normaliseLineBreaks(text)
        guard CallChatText.isSendable(normalised), let sendAction else { return false }
        let result = sendAction(normalised)
        messages.append(CallChatMessage(
            id: "own-\(result.seq)", sender: .own, seq: result.seq, text: normalised,
            arrivedAt: now(), delivery: result.sent ? .sent : .notSent))
        scrollRequest += 1
        return true
    }

    /// `Send again` on a message that did not go out.
    func sendAgain(_ id: String) {
        guard let index = messages.firstIndex(where: { $0.id == id }),
              messages[index].isOwn, messages[index].delivery == .notSent,
              let resendAction
        else { return }
        if resendAction(messages[index].seq) {
            messages[index].delivery = .sent
        }
    }

    // MARK: Receiving

    /// A verified message from `peerId`.
    func receive(peerId: String, seq: UInt64, text: String) {
        let message = CallChatMessage(
            id: "\(peerId)-\(seq)", sender: .peer(peerId), seq: seq, text: text,
            arrivedAt: now(), delivery: .sent)
        guard !messages.contains(where: { $0.id == message.id }) else { return }
        messages.append(message)
        let name = senderName(peerId)
        if panelOpen {
            if atBottom { scrollRequest += 1 } else { newBelow = true }
            announce(Localization.text("call.chat.message_a11y", texts: ["name": name, "text": text]))
        } else {
            unread += 1
            previewCounter += 1
            let shown = Preview(id: previewCounter, peerId: peerId, text: text)
            preview = shown
            schedule(Self.previewSeconds) { [weak self] in self?.expirePreview(shown.id) }
        }
    }

    /// The preview numbered `id` ran out (a newer one replaced it already
    /// when the numbers differ).
    func expirePreview(_ id: Int) {
        if preview?.id == id { preview = nil }
    }

    // MARK: The panel

    func openPanel() {
        panelOpen = true
        unread = 0
        preview = nil
        atBottom = true
        newBelow = false
        scrollRequest += 1
    }

    func closePanel() {
        panelOpen = false
    }

    func togglePanel() {
        if panelOpen { closePanel() } else { openPanel() }
    }

    /// The list reached (or left) its newest message.
    func setAtBottom(_ bottom: Bool) {
        atBottom = bottom
        if bottom { newBelow = false }
    }

    // MARK: Names

    /// The name over `peerId`'s bubbles: the one their tile shows
    /// (`docs/ux/group-call-grid.md` / `CallTileName`), following a later
    /// change, and the last one it showed once they left.
    func senderName(_ peerId: String) -> String {
        if let live = tileName(peerId)?.trimmingCharacters(in: .whitespacesAndNewlines), !live.isEmpty {
            lastNames[peerId] = live
            return live
        }
        return lastNames[peerId] ?? CallPeerName.tileLabel(displayName: "", isLocal: false)
    }

    // MARK: The call's end

    /// The call ended for this device: every message goes and the panel
    /// closes. The chat stays available for the screen still on show.
    func callEnded() {
        messages = []
        panelOpen = false
        unread = 0
        preview = nil
        newBelow = false
        atBottom = true
        lastNames = [:]
    }

    /// A fresh call: nothing of the last one, and no chat until an adapter
    /// wires one.
    func reset() {
        callEnded()
        available = false
        sendAction = nil
        resendAction = nil
    }

    // MARK: Rules the views read

    /// The badge on the `Chat` circle: `nil` for none, `1` to `9`, then `9+`.
    static func badgeText(unread: Int) -> String? {
        guard unread > 0 else { return nil }
        return unread > 9 ? "9+" : "\(unread)"
    }

    /// The `Chat` circle's spoken name.
    static func spokenCircleLabel(open: Bool, unread: Int) -> String {
        if open { return Localization.text("call.control.chat_hide_a11y") }
        if unread > 0 {
            return Localization.text("call.control.chat_unread_a11y", counts: ["count": UInt64(unread)])
        }
        return Localization.text("call.control.chat_show_a11y")
    }

    /// Whether the message at `index` starts a run from another person, so
    /// their name goes above it.
    static func startsRun(at index: Int, in messages: [CallChatMessage]) -> Bool {
        guard messages.indices.contains(index), !messages[index].isOwn else { return false }
        return index == 0 || messages[index - 1].sender != messages[index].sender
    }

    /// The panel is the end-side column rather than a panel over the whole
    /// screen (`docs/ux/calls.md` section 4.1.4).
    static func isColumn(screenWidth: CGFloat) -> Bool { screenWidth >= 900 }

    /// The column's width.
    static let columnWidth: CGFloat = 360
}
