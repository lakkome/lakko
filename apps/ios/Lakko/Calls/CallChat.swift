// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import ImageIO
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

    /// A verified message: its sequence number and its text, or, for a
    /// picture (section 4.1.4.1), the picture with an empty text.
    struct Verified: Equatable {
        let seq: UInt64
        let text: String
        var picture: CallChatPicture? = nil
    }

    /// What one own message carries.
    enum Content: Equatable {
        case text(String)
        case picture(CallChatPicture)
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
        /// This device's signed picture message as `selfPeerId`, numbered
        /// `seq` (section 4.1.4.1), or `nil` when it cannot make one yet.
        var composePicture: @MainActor (
            _ selfPeerId: String, _ seq: UInt64, _ picture: CallChatPicture
        ) -> Data? = { _, _, _ in nil }
    }

    /// At most this many messages are held for one peer whose announcement
    /// has not verified yet; the oldest goes first. Chosen, not measured.
    static let maxPendingPerPeer = 50

    private let deps: Deps
    /// The last sequence number this device used (numbers start at 1).
    private var ownSeq: UInt64 = 0
    /// Own messages that did not go out, by sequence number: the content,
    /// and the signed bytes once they could be made. ``resend(seq:)`` sends
    /// the same bytes again.
    private var unsent: [UInt64: (content: Content, wire: Data?)] = [:]
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
        let seq = reserveSeq()
        return (seq, send(.text(text), seq: seq))
    }

    /// The next sequence number of this device's messages, taken now for a
    /// message that goes out later: a picture waits for its upload, and text
    /// typed with it follows it (section 4.1.4.1). A picture and a text share
    /// one counter.
    func reserveSeq() -> UInt64 {
        ownSeq += 1
        return ownSeq
    }

    /// Send `content` under the number ``reserveSeq()`` gave it. Returns
    /// whether it went out; one that did not is kept for ``resend(seq:)``.
    func send(_ content: Content, seq: UInt64) -> Bool {
        let wire = deps.selfPeerId().flatMap { compose(content, selfPeerId: $0, seq: seq) }
        if let wire, deps.sendSealed(wire) {
            unsent.removeValue(forKey: seq)
            return true
        }
        unsent[seq] = (content, wire)
        return false
    }

    /// Send the message numbered `seq` again: the same signed bytes, or, when
    /// none could be made at the time, the same content under the same
    /// number. Returns whether it went out this time.
    func resend(seq: UInt64) -> Bool {
        guard let held = unsent[seq] else { return false }
        var wire = held.wire
        if wire == nil {
            wire = deps.selfPeerId().flatMap { compose(held.content, selfPeerId: $0, seq: seq) }
            unsent[seq] = (held.content, wire)
        }
        guard let wire, deps.sendSealed(wire) else { return false }
        unsent.removeValue(forKey: seq)
        return true
    }

    private func compose(_ content: Content, selfPeerId: String, seq: UInt64) -> Data? {
        switch content {
        case .text(let text): return deps.compose(selfPeerId, seq, text)
        case .picture(let picture): return deps.composePicture(selfPeerId, seq, picture)
        }
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

    /// The draft after the emoji picker puts `emoji` at its end
    /// (`docs/ux/calls.md` section 4.1.4, "The emoji button"). An emoji that
    /// would pass the limit is not put in, not even in part: the draft stays
    /// as it was and the too-long line shows.
    static func appendingEmoji(_ emoji: String, to draft: String) -> (text: String, tooLong: Bool) {
        let current = normaliseLineBreaks(draft)
        let candidate = current + emoji
        let edited = edited(candidate)
        if edited.tooLong {
            return (current, true)
        }
        return edited
    }

    /// The draft after the emoji picker puts `emoji` at the caret: over the
    /// selection `range` (UTF-16 offsets into `draft`), or at the insertion
    /// point when the range is empty. With no range, or one that no longer
    /// fits the draft or falls inside a character, the emoji goes at the end
    /// (``appendingEmoji(_:to:)``). The same limit rule: an emoji that would
    /// pass 800 bytes is not put in and the draft stays as it was.
    ///
    /// `caret` is the UTF-16 offset just after the emoji, or `nil` when
    /// nothing was put in.
    static func insertingEmoji(
        _ emoji: String, into draft: String, range: Range<Int>?
    ) -> (text: String, tooLong: Bool, caret: Int?) {
        let current = normaliseLineBreaks(draft)
        let utf16 = current.utf16
        guard let range, range.lowerBound >= 0, range.upperBound <= utf16.count,
              let lower = utf16.index(utf16.startIndex, offsetBy: range.lowerBound, limitedBy: utf16.endIndex)?
                .samePosition(in: current),
              let upper = utf16.index(utf16.startIndex, offsetBy: range.upperBound, limitedBy: utf16.endIndex)?
                .samePosition(in: current) else {
            let appended = appendingEmoji(emoji, to: current)
            return (appended.text, appended.tooLong, appended.tooLong ? nil : appended.text.utf16.count)
        }
        var candidate = current
        candidate.replaceSubrange(lower..<upper, with: emoji)
        let edited = edited(candidate)
        if edited.tooLong {
            return (current, true, nil)
        }
        return (edited.text, false, range.lowerBound + emoji.utf16.count)
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
        /// This device's message is on its way: a picture whose file
        /// uploads, or text waiting for the picture sent before it
        /// (section 4.1.4.1). Dimmed; a picture also shows a spinner.
        case sending
        /// Not sent: the bubble is dimmed and offers `Send again`.
        case notSent
    }

    /// A picture message (section 4.1.4.1): its pixel size and where its
    /// bytes are. The bytes themselves live in the model, in memory only.
    struct Picture: Equatable {
        enum State: Equatable {
            /// Downloading or opening.
            case loading
            /// The model holds its bytes and image.
            case ready
            /// It could not be downloaded or opened.
            case unavailable
        }

        let width: UInt16
        let height: UInt16
        var state: State
    }

    let id: String
    let sender: Sender
    let seq: UInt64
    /// The text; empty for a picture.
    let text: String
    /// When it arrived on (or was written on) this device.
    let arrivedAt: Date
    var delivery: Delivery
    /// The picture, for a picture message.
    var picture: Picture? = nil

    var isOwn: Bool { sender == .own }
}

/// A pasted picture made ready to send (section 4.1.4.1): re-encoded as a
/// JPEG with its longest side at most the conversation's limit, which drops
/// every piece of metadata.
struct CallChatPreparedPicture: Equatable {
    let jpeg: Data
    let width: UInt16
    let height: UInt16
}

/// The rules of pictures in the call chat (`docs/ux/calls.md` section
/// 4.1.4.1), pure apart from the image decoding.
enum CallChatPictures {
    /// The idle life a picture's file asks the file store for, in days.
    static let idleDays: UInt8 = 1
    /// The bubble's box: at most this on either side.
    static let bubbleMax: CGFloat = 240
    /// ... and at least this on either side.
    static let bubbleMin: CGFloat = 80
    /// The tray's thumbnail height.
    static let trayHeight: CGFloat = 64

    /// Why a pasted item does not go into the tray.
    enum Refusal: Error, Equatable {
        /// It does not decode as a picture.
        case notPicture
        /// Larger than the core's bound after preparing.
        case tooLarge

        var textKey: String {
            switch self {
            case .notPicture: return "call.chat.picture_only"
            case .tooLarge: return "call.chat.picture_too_large"
            }
        }
    }

    /// One part of what Send sends, in the order it goes out.
    enum Part: Equatable {
        case picture
        case text(String)
    }

    /// What Send sends for this draft and tray: the picture first, then the
    /// text as a message of its own when there is any. Empty when nothing
    /// can be sent.
    static func parts(draft: String, hasPicture: Bool) -> [Part] {
        let text = CallChatText.normaliseLineBreaks(draft)
        let textSendable = CallChatText.isSendable(text)
        var out: [Part] = []
        if hasPicture { out.append(.picture) }
        if textSendable { out.append(.text(text)) }
        return out
    }

    /// Whether Send is enabled: a picture in the tray, or sendable text.
    static func canSend(draft: String, hasPicture: Bool) -> Bool {
        !parts(draft: draft, hasPicture: hasPicture).isEmpty
    }

    /// The bubble's box for a `width` x `height` picture: fitted into 240 x
    /// 240 keeping its shape (never enlarged), then each side at least 80; a
    /// very long or very tall picture is cropped by the box. The web's
    /// `pictureBox`.
    static func bubbleSize(width: UInt16, height: UInt16) -> CGSize {
        guard width > 0, height > 0 else { return CGSize(width: bubbleMax, height: bubbleMax) }
        let w = CGFloat(width)
        let h = CGFloat(height)
        let scale = min(bubbleMax / w, bubbleMax / h, 1)
        return CGSize(
            width: max(bubbleMin, (w * scale).rounded()),
            height: max(bubbleMin, (h * scale).rounded()))
    }

    /// Decode `data`, bring its longest side down to `maxSide`, re-encode it
    /// as JPEG. Not on the main actor: decoding a large picture takes time.
    static func prepare(
        _ data: Data,
        maxSide: Int = Int(photoMaxLongestSidePx()),
        maxBytes: Int = Int(callChatMaxPictureBytes())
    ) -> Result<CallChatPreparedPicture, Refusal> {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              CGImageSourceGetCount(source) > 0,
              let jpeg = PhotoVideoPrep.transcodeToJPEG(source, maxSide: maxSide),
              let encoded = CGImageSourceCreateWithData(jpeg as CFData, nil),
              let size = PhotoVideoPrep.orientedPixelSize(encoded),
              size.width > 0, size.height > 0,
              size.width <= Int(UInt16.max), size.height <= Int(UInt16.max)
        else { return .failure(.notPicture) }
        guard jpeg.count <= maxBytes else { return .failure(.tooLarge) }
        return .success(CallChatPreparedPicture(
            jpeg: jpeg, width: UInt16(size.width), height: UInt16(size.height)))
    }
}

extension CallChatPictures {
    /// The first picture among pasted items, prepared, or why there is none:
    /// a paste with no picture in it (a video) is `notPicture`.
    static func loadPasted(_ providers: [NSItemProvider]) async -> Result<CallChatPreparedPicture, Refusal> {
        let picture = providers.first { provider in
            switch PastedMedia.itemKind(registered: provider.registeredTypeIdentifiers) {
            case .photo, .uiImage: return true
            case .video, nil: return false
            }
        }
        guard let picture,
              case .photo(let data)? = await PastedMedia.load([picture]).first
        else { return .failure(.notPicture) }
        return await Task.detached(priority: .userInitiated) { CallChatPictures.prepare(data) }.value
    }
}

/// The file store side of the call chat's pictures (section 4.1.4.1).
/// Replaced in tests.
struct CallChatPictureIO {
    /// Encrypt `picture` with a fresh key and upload it asking for a one-day
    /// idle life; what the message then carries.
    var upload: @MainActor (_ picture: CallChatPreparedPicture) async throws -> CallChatPicture
    /// Download and decrypt a picture.
    var download: @MainActor (_ picture: CallChatPicture) async throws -> Data
    /// Delete these of this device's uploads: the owed delete a message
    /// deleted for everyone uses (ADR-0065), kept until the store confirms.
    var deleteUploaded: @MainActor (_ blobIds: [Data]) -> Void

    static let live = CallChatPictureIO(
        upload: { picture in
            let ref = try await AttachmentBridge.encryptAndUpload(
                bytes: picture.jpeg, mime: "image/jpeg", name: nil,
                idleDays: CallChatPictures.idleDays)
            return CallChatPicture(
                blobId: ref.blobId, key: ref.key, nonce: ref.nonce,
                size: UInt32(picture.jpeg.count), width: picture.width, height: picture.height)
        },
        download: { picture in
            try await AttachmentBridge.downloadAndDecrypt(ref: AttachmentRef(
                blobId: picture.blobId, key: picture.key, nonce: picture.nonce,
                mime: "image/jpeg", size: UInt64(picture.size), name: nil))
        },
        deleteUploaded: { blobIds in BlobLifecycle.shared.delete(blobIds) }
    )
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

    // MARK: Pictures (section 4.1.4.1)

    /// The call can send pictures: its adapter wired the picture actions.
    @Published private(set) var picturesAvailable = false
    /// The picture waiting in the tray above the field: one at a time, a
    /// second paste replaces it.
    @Published private(set) var trayPicture: CallChatPreparedPicture?
    /// The tray's thumbnail.
    @Published private(set) var trayImage: UIImage?
    /// Why the last pasted item did not go into the tray; the line under the
    /// field shows it until the next paste or send.
    @Published private(set) var pictureRefusal: CallChatPictures.Refusal?
    /// Every picture's image on the list, by message id, in memory only.
    @Published private(set) var pictureImages: [String: UIImage] = [:]
    /// The file store side; replaced in tests.
    var pictureIO = CallChatPictureIO.live
    /// Every picture's JPEG bytes, by message id: what the viewer and
    /// `Save picture` read, and what an upload that never finished retries.
    private var pictureBytes: [String: Data] = [:]
    /// This device's pictures by sequence number: what each carries once its
    /// file is up.
    private var ownPictures: [UInt64: (picture: CallChatPreparedPicture, uploaded: CallChatPicture?)] = [:]
    /// The files this device uploaded in this call, deleted when it ends.
    private(set) var uploadedBlobIds: [Data] = []
    /// Bumped when the call ends, so work that finishes later does nothing.
    private var generation = 0

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
    private var reserveSeqAction: (() -> UInt64)?
    private var sendContentAction: ((CallChatChannel.Content, UInt64) -> Bool)?
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

    /// The call's adapter for pictures (section 4.1.4.1): numbers are taken
    /// with `reserveSeq` and the content goes out with `send` under its
    /// number, so a picture that waits for its upload keeps its place and
    /// text typed with it follows it.
    func setPictureActions(
        reserveSeq: @escaping () -> UInt64,
        send: @escaping (CallChatChannel.Content, UInt64) -> Bool
    ) {
        reserveSeqAction = reserveSeq
        sendContentAction = send
        picturesAvailable = true
    }

    // MARK: Sending

    /// Send `text`. Returns whether the composer should empty: `false` when
    /// the text is not sendable or the call carries no chat.
    @discardableResult
    func send(_ text: String) -> Bool {
        if trayPicture != nil { return sendWithPicture(text) }
        let normalised = CallChatText.normaliseLineBreaks(text)
        guard CallChatText.isSendable(normalised), let sendAction else { return false }
        let result = sendAction(normalised)
        messages.append(CallChatMessage(
            id: "own-\(result.seq)", sender: .own, seq: result.seq, text: normalised,
            arrivedAt: now(), delivery: result.sent ? .sent : .notSent))
        scrollRequest += 1
        return true
    }

    /// `Send again` on a message that did not go out. A picture whose file
    /// never finished uploading is uploaded again; any other message goes
    /// out again as the same signed bytes.
    func sendAgain(_ id: String) {
        guard let index = messages.firstIndex(where: { $0.id == id }),
              messages[index].isOwn, messages[index].delivery == .notSent
        else { return }
        let seq = messages[index].seq
        if messages[index].picture != nil, let own = ownPictures[seq], own.uploaded == nil {
            messages[index].delivery = .sending
            let generation = generation
            Task { @MainActor [weak self] in
                _ = await self?.deliverPicture(seq: seq, generation: generation)
            }
            return
        }
        guard let resendAction else { return }
        if resendAction(seq) {
            messages[index].delivery = .sent
        }
    }

    /// Send the tray's picture, then `text` as its own message when there is
    /// any (section 4.1.4.1). Both bubbles show at once; the picture goes
    /// out once its file is up, and the text after it, so everyone receives
    /// them in this order.
    private func sendWithPicture(_ text: String) -> Bool {
        guard let picture = trayPicture, let reserveSeqAction else { return false }
        let parts = CallChatPictures.parts(draft: text, hasPicture: true)
        trayPicture = nil
        trayImage = nil
        pictureRefusal = nil
        let pictureSeq = reserveSeqAction()
        let pictureId = "own-\(pictureSeq)"
        ownPictures[pictureSeq] = (picture, nil)
        pictureBytes[pictureId] = picture.jpeg
        pictureImages[pictureId] = UIImage(data: picture.jpeg)
        messages.append(CallChatMessage(
            id: pictureId, sender: .own, seq: pictureSeq, text: "", arrivedAt: now(),
            delivery: .sending,
            picture: .init(width: picture.width, height: picture.height, state: .ready)))
        var following: (seq: UInt64, text: String)?
        for case .text(let body) in parts {
            let seq = reserveSeqAction()
            following = (seq, body)
            messages.append(CallChatMessage(
                id: "own-\(seq)", sender: .own, seq: seq, text: body, arrivedAt: now(),
                delivery: .sending))
        }
        scrollRequest += 1
        let generation = generation
        Task { @MainActor [weak self] in
            guard let self else { return }
            _ = await self.deliverPicture(seq: pictureSeq, generation: generation)
            guard let following, generation == self.generation else { return }
            let sent = self.sendContentAction?(.text(following.text), following.seq) ?? false
            self.setDelivery("own-\(following.seq)", sent ? .sent : .notSent)
        }
        return true
    }

    /// Upload the own picture numbered `seq` if its file is not up yet, then
    /// send its message. Returns whether the message went out.
    private func deliverPicture(seq: UInt64, generation: Int) async -> Bool {
        guard var own = ownPictures[seq] else { return false }
        let id = "own-\(seq)"
        if own.uploaded == nil {
            do {
                let uploaded = try await pictureIO.upload(own.picture)
                guard generation == self.generation else {
                    // The call ended while it uploaded: the file goes now.
                    pictureIO.deleteUploaded([uploaded.blobId])
                    return false
                }
                own.uploaded = uploaded
                ownPictures[seq] = own
                uploadedBlobIds.append(uploaded.blobId)
            } catch {
                guard generation == self.generation else { return false }
                NSLog("CallChatModel: a picture did not upload")
                setDelivery(id, .notSent)
                return false
            }
        }
        guard let uploaded = own.uploaded else { return false }
        let sent = sendContentAction?(.picture(uploaded), seq) ?? false
        setDelivery(id, sent ? .sent : .notSent)
        return sent
    }

    private func setDelivery(_ id: String, _ delivery: CallChatMessage.Delivery) {
        guard let index = messages.firstIndex(where: { $0.id == id }) else { return }
        messages[index].delivery = delivery
    }

    // MARK: The tray

    /// A pasted picture, prepared (or refused): into the tray, replacing the
    /// one there, or the refusal line under the field with the tray as it
    /// was.
    func pasted(_ result: Result<CallChatPreparedPicture, CallChatPictures.Refusal>) {
        switch result {
        case .success(let picture):
            trayPicture = picture
            trayImage = UIImage(data: picture.jpeg)
            pictureRefusal = nil
        case .failure(let refusal):
            pictureRefusal = refusal
        }
    }

    /// The tray's cross.
    func removeTrayPicture() {
        trayPicture = nil
        trayImage = nil
    }

    /// Whether Send is enabled for `draft`.
    func canSend(_ draft: String) -> Bool {
        CallChatPictures.canSend(draft: draft, hasPicture: trayPicture != nil)
    }

    /// The JPEG bytes of the picture message `id`, once on this device.
    func pictureData(_ id: String) -> Data? { pictureBytes[id] }

    // MARK: Receiving

    /// A verified message from `peerId`.
    func receive(peerId: String, seq: UInt64, text: String) {
        let message = CallChatMessage(
            id: "\(peerId)-\(seq)", sender: .peer(peerId), seq: seq, text: text,
            arrivedAt: now(), delivery: .sent)
        guard !messages.contains(where: { $0.id == message.id }) else { return }
        messages.append(message)
        arrived(peerId: peerId, text: text)
    }

    /// A verified picture from `peerId` (section 4.1.4.1): shown at once as
    /// a box of its shape while it downloads, then the picture, or `Picture
    /// not available.` when it cannot be downloaded or opened.
    func receivePicture(peerId: String, seq: UInt64, picture: CallChatPicture) {
        let id = "\(peerId)-\(seq)"
        guard !messages.contains(where: { $0.id == id }) else { return }
        messages.append(CallChatMessage(
            id: id, sender: .peer(peerId), seq: seq, text: "", arrivedAt: now(), delivery: .sent,
            picture: .init(width: picture.width, height: picture.height, state: .loading)))
        arrived(peerId: peerId, text: Localization.text("call.chat.picture"))
        let generation = generation
        Task { @MainActor [weak self] in
            guard let self else { return }
            let data = try? await self.pictureIO.download(picture)
            guard generation == self.generation,
                  let index = self.messages.firstIndex(where: { $0.id == id }) else { return }
            if let data, let image = UIImage(data: data) {
                self.pictureBytes[id] = data
                self.pictureImages[id] = image
                self.messages[index].picture?.state = .ready
            } else {
                NSLog("CallChatModel: a picture is not available")
                self.messages[index].picture?.state = .unavailable
            }
        }
    }

    /// The badge, the preview or the announcement for a message from
    /// `peerId` whose text (or `Picture`) is `text`.
    private func arrived(peerId: String, text: String) {
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
        // This device's uploads go with the call (section 4.1.4.1), through
        // the owed delete; an upload still running is deleted when it ends.
        generation += 1
        if !uploadedBlobIds.isEmpty { pictureIO.deleteUploaded(uploadedBlobIds) }
        uploadedBlobIds = []
        ownPictures = [:]
        pictureBytes = [:]
        pictureImages = [:]
        trayPicture = nil
        trayImage = nil
        pictureRefusal = nil
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
        picturesAvailable = false
        reserveSeqAction = nil
        sendContentAction = nil
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
