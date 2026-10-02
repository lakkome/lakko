// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import Foundation

/// The link to a call that is already running (`docs/ux/call-links.md`).
///
/// ## What this replaced
///
/// A form. `Create call link` asked four questions — audio or video, when the
/// link expires, how many uses, what to label it — and produced a string of
/// text the person then carried somewhere else themselves. The link was an
/// object you built before there was anything to join.
///
/// It is now a property of a call that is already happening: the person lands
/// in the call, and copies a link from inside it when they want to bring in
/// someone who is not in their contacts. Nothing is asked, because there is
/// nothing left to decide — the values are fixed in ``CallInviteValues`` and
/// the boundary is the host's admission, not the link's secrecy.

// MARK: - The values every link carries

/// The four fields `CallInviteCapability` carries that the person is no longer
/// asked about (`docs/ux/call-links.md` section 4).
///
/// They are constants rather than arguments because there is one kind of call
/// link now. A caller that could pass something else would be a question in
/// disguise, and the three clients would drift apart again on what the answers
/// mean — which is exactly what happened to the form this deletes.
public enum CallInviteValues {

    /// Every call carries audio.
    public static let audio = true

    /// And video: one kind of call. A voice call is a video call with the
    /// camera off, which the in-call controls already do.
    public static let video = true

    /// Unbounded, from the shared value. The host admits each arrival one at
    /// a time, so a count here would cap a meeting for no gain.
    public static var maxUses: UInt32 { LakkoCore.callInviteMaxUses() }

    /// Empty. Nothing to say that the call screen does not.
    public static let label = ""

    /// The wire `expiry_ms` for a link minted at `nowMs`, from the shared
    /// rule.
    ///
    /// Saturating, and never the `0` sentinel - both guaranteed by the shared
    /// implementation rather than re-checked here.
    public static func expiryMs(nowMs: UInt64) -> UInt64 {
        LakkoCore.callInviteExpiryMs(nowMs: nowMs)
    }

    /// The `expiry_ms` for a link minted now.
    public static func expiryMs() -> UInt64 {
        expiryMs(nowMs: UInt64(Date().timeIntervalSince1970 * 1000))
    }

    /// Length of the `call_id` a call is named by.
    public static let callIdLen = 16

    /// A fresh `call_id` for a call this device is about to found.
    ///
    /// `New call` needs no capability to start a call — ``MultiJoinerCall``
    /// takes the id and the owner's own keys as plain arguments — so the id is
    /// minted here, in the open, and a link for the SAME id is minted later
    /// only if someone asks for one.
    public static func freshCallId() -> Data {
        var callId = Data(count: callIdLen)
        callId.withUnsafeMutableBytes { raw in
            guard let base = raw.baseAddress else { return }
            if SecRandomCopyBytes(kSecRandomDefault, callIdLen, base) != errSecSuccess {
                arc4random_buf(base, callIdLen)
            }
        }
        return callId
    }
}

// MARK: - The offer's state

/// Where the link ended up.
public enum CallInviteDelivery: Equatable {
    /// On the clipboard.
    case copied
    /// The clipboard was not reachable, so the link itself is shown to be
    /// copied by hand.
    case shown
    /// Sent as an invitation into a conversation; `name` is its title.
    case sent(name: String)
}

/// Why `Invite` did not do what was asked.
public enum CallInviteFailure: Equatable {
    /// There is no live session to mint under.
    case signedOut
    /// The round trip failed, or the ledger refused.
    case unavailable
    /// The link exists, but the invitation could not be sent to the picked
    /// conversation.
    case notSent
}

/// What `Invite` is doing, from the moment the call starts. The twin of the
/// web `InCallInvite` (`apps/web/src/calls/in-call-invite.ts`).
public enum CallInviteOfferState: Equatable {
    /// Nobody has asked for a link yet, so none has been made. This is the
    /// state every call starts in, and the reason starting a call is instant.
    case idle
    /// Being made. A round trip and an anti-spam check, so it is not instant
    /// and the screen has to say what it is doing.
    case minting
    /// A link exists for this call, and where the last use put it.
    case ready(url: String, delivery: CallInviteDelivery)
    /// The last attempt failed. The call is untouched. `url` is kept when the
    /// link was minted and only the delivery failed, so the next use does not
    /// mint again.
    case failed(failure: CallInviteFailure, url: String?)
}

/// What one use of `Invite` does, and what the screen says while it happens.
///
/// Pure, because a SwiftUI body is the one place in this app a test cannot
/// reach — the same reason ``CallScreenPresentation`` exists.
public enum CallInviteOfferRules {

    /// What choosing a menu item does now.
    public enum Press: Equatable {
        /// No link yet: make one, then deliver it.
        case mint
        /// There is one already: deliver it, without a second round trip.
        case reoffer
        /// One is being made right now. A second use must not start a second
        /// mint — the link would be a different link for the same call.
        case ignore
    }

    /// The link this call already has, if any.
    public static func mintedUrl(_ state: CallInviteOfferState) -> String? {
        switch state {
        case .ready(let url, _): return url
        case .failed(_, let url): return url
        case .idle, .minting: return nil
        }
    }

    /// What the next use of `Invite` should do.
    ///
    /// A failed mint returns to `mint`: nothing about a failure makes the
    /// next attempt less likely to work. A failed SEND keeps the link it
    /// already has.
    public static func nextPress(_ state: CallInviteOfferState) -> Press {
        switch state {
        case .minting: return .ignore
        case .ready: return .reoffer
        case .idle: return .mint
        case .failed(_, let url): return url == nil ? .mint : .reoffer
        }
    }

    /// Whether the control shows its working state.
    public static func busy(_ state: CallInviteOfferState) -> Bool {
        state == .minting
    }

    /// The line the call screen shows under the status row, or `nil` when
    /// there is nothing to say.
    public static func statusLine(_ state: CallInviteOfferState) -> String? {
        switch state {
        case .idle:
            return nil
        case .minting:
            return mintingMessage
        case .ready(let url, let delivery):
            switch delivery {
            case .copied: return Localization.text("call.invite.copied")
            case .shown: return Localization.text("call.invite.shown", texts: ["url": url])
            case .sent(let name): return Localization.text("call.invite.sent", texts: ["name": name])
            }
        case .failed(let failure, _):
            switch failure {
            case .signedOut: return signedOutMessage
            case .unavailable: return Localization.text("call.invite.unavailable")
            case .notSent: return sendFailedMessage
            }
        }
    }

    /// How long a finished line stays up before the screen goes quiet again.
    public static let statusHoldMs: UInt64 = 6_000

    /// How long this state's line should stay, or `nil` for "until something
    /// else replaces it".
    ///
    /// `minting` has no timer: it ends when the mint does. `shown` has none
    /// either — taking a link away while someone is copying it out is the one
    /// case where a timer does harm.
    public static func statusHoldMs(_ state: CallInviteOfferState) -> UInt64? {
        switch state {
        case .minting, .idle: return nil
        case .ready(_, .shown): return nil
        case .ready, .failed: return statusHoldMs
        }
    }

    /// Put the link on the clipboard, or in front of the person to copy by
    /// hand when the clipboard is not reachable.
    public static func copyLink(
        _ url: String, copy: ((String) throws -> Void)?
    ) -> CallInviteDelivery {
        guard let copy else { return .shown }
        do {
            try copy(url)
            return .copied
        } catch {
            return .shown
        }
    }

    /// The word under the control. It names the act, never the state, so it
    /// does not change size or meaning when the state flips.
    public static var controlLabel: String { Localization.text("call.control.invite") }

    /// What the busy state says. The spec is explicit that it says what it is
    /// doing rather than only spinning: a spinner for a few seconds with no
    /// words reads as a stuck screen.
    public static var mintingMessage: String { Localization.text("call.invite.minting") }

    /// What a failed send says, in the picker and on the call screen.
    public static var sendFailedMessage: String { Localization.text("call.invite.send_failed") }

    /// The kind of a mint failure. The failure's own prose goes to the log
    /// only (ADR-0061).
    public static func failure(for error: Error) -> CallInviteFailure {
        if case LinkCallHostError.signedOut = error { return .signedOut }
        return .unavailable
    }

    /// One sentence on what happened, one on what to do. Never ends the call,
    /// and says so, because a failure inside a live call reads as the call
    /// having broken when nothing has.
    public static func failureMessage(for error: Error) -> String {
        statusLine(.failed(failure: failure(for: error), url: nil)) ?? ""
    }

    /// What a signed-out device says. Section 5's last row: one sentence.
    public static var signedOutMessage: String { Localization.text("call.invite.signed_out") }
}

// MARK: - The offer

/// A conversation an invitation can be sent into.
public struct CallInviteTarget: Equatable {
    /// The conversation's id (a group id, hex).
    public let id: String
    /// Its title, as the conversation list shows it.
    public let title: String

    public init(id: String, title: String) {
        self.id = id
        self.title = title
    }
}

/// Inviting to one live call (`docs/ux/call-links.md` section 3): the app's
/// own menu with **Copy invite link** and **Invite contacts**, never the
/// system's share sheet.
///
/// Both items hand out the link to THIS call. The first use in a call is what
/// mints it; later uses reuse the link already minted, and a send that failed
/// keeps it too.
///
/// Owned by ``LinkCallHost`` for the duration of a call this device founded,
/// so the second use in a call finds the link the first one made, and a new
/// call starts with no link at all.
///
/// Only the call's founder holds one. A capability names its signer as the
/// call's owner, and ADR-0032 elects the committer from that owner, so a link
/// minted by anyone else would send arrivals to a call nobody is founding.
@MainActor
public final class CallInviteOffer: ObservableObject {

    @Published public private(set) var state: CallInviteOfferState = .idle
    /// The menu above the control bar is open.
    @Published public private(set) var menuOpen = false
    /// The conversation picker (`Invite contacts`) is open.
    @Published public private(set) var pickerOpen = false
    /// Why the last send did not go, shown in the picker, which stays open.
    @Published public private(set) var pickerError: String?
    /// The conversations an invitation has been sent into during this call.
    /// Their rows show a check and cannot be picked again, so the picker can
    /// stay open while several people are invited.
    @Published public private(set) var invited: Set<String> = []
    /// A send is in flight; the picker refuses a second pick.
    @Published public private(set) var sending = false
    /// Whether the state's line is on the call screen right now.
    @Published private var lineShown = false

    private let mint: @MainActor () async throws -> String
    private let copy: ((String) throws -> Void)?
    private let send: @MainActor (_ conversationId: String, _ text: String) async throws -> Void
    private let conversations: @MainActor () -> [ConversationListRow]
    private let hold: @MainActor (_ ms: UInt64) async -> Void
    private var holdTask: Task<Void, Never>?

    /// - Parameters:
    ///   - mint: makes the link for THIS call and returns its URL.
    ///   - copy: puts a string on the clipboard; `nil` when there is none.
    ///   - send: sends a plain text message into a conversation through the
    ///     normal send path. Throws when it could not be sent.
    ///   - conversations: the conversations an invitation can be sent into,
    ///     the list `Forward to` shows.
    ///   - hold: waits out a status line's time on screen.
    public init(
        mint: @MainActor @escaping () async throws -> String,
        copy: ((String) throws -> Void)? = nil,
        conversations: @MainActor @escaping () -> [ConversationListRow] = { [] },
        send: @MainActor @escaping (_ conversationId: String, _ text: String) async throws -> Void = { _, _ in
            throw CallInviteOfferError.noSendPath
        },
        hold: @MainActor @escaping (_ ms: UInt64) async -> Void = { ms in
            try? await Task.sleep(nanoseconds: ms * 1_000_000)
        }
    ) {
        self.mint = mint
        self.copy = copy
        self.send = send
        self.conversations = conversations
        self.hold = hold
    }

    /// The conversations an invitation can be sent into, in the conversation
    /// list's own order. Empty for someone with none (a guest who came by a
    /// link), and then `Invite contacts` is absent from the menu.
    public func targets() -> [ConversationListRow] { conversations() }

    /// The URL, once there is one.
    public var url: String? { CallInviteOfferRules.mintedUrl(state) }

    /// The link is being prepared: the control shows a spinner and refuses a
    /// second use rather than paying for a second link.
    public var busy: Bool { CallInviteOfferRules.busy(state) }

    /// What the call screen shows under its status row, or `nil`.
    public var statusLine: String? {
        lineShown ? CallInviteOfferRules.statusLine(state) : nil
    }

    // MARK: The menu and the picker

    /// `Participants` was pressed: open the menu, or close it.
    public func toggleMenu() {
        menuOpen.toggle()
    }

    public func closeMenu() {
        if menuOpen { menuOpen = false }
    }

    /// `Invite contacts` was chosen.
    public func openPicker() {
        menuOpen = false
        pickerError = nil
        pickerOpen = true
    }

    public func closePicker() {
        pickerOpen = false
        pickerError = nil
    }

    // MARK: The two items

    /// `Copy link`: the link goes to the clipboard, and the line says so.
    public func copyLink() async {
        menuOpen = false
        guard let url = await linkForCall() else { return }
        show(.ready(url: url, delivery: CallInviteOfferRules.copyLink(url, copy: copy)))
    }

    /// Send the invitation into the picked conversation.
    ///
    /// The message is the call's link and nothing else, which is what lets
    /// every client draw it as an invitation with a `Join` button
    /// (``CallInviteLink``). The picker stays open either way, so several
    /// people can be invited: a sent row is marked and cannot be picked
    /// again; a failed one stays pressable under the reason, and the link is
    /// kept, so the next attempt does not mint a second one. The call is
    /// untouched either way.
    ///
    /// - Returns: whether the invitation was sent.
    @discardableResult
    public func sendInvite(to target: CallInviteTarget) async -> Bool {
        guard !sending, !invited.contains(target.id) else { return false }
        sending = true
        defer { sending = false }
        pickerError = nil
        guard let url = await linkForCall() else {
            pickerError = CallInviteOfferRules.sendFailedMessage
            return false
        }
        do {
            try await send(target.id, url)
        } catch {
            NSLog("call invite: the invitation was not sent")
            show(.failed(failure: .notSent, url: url))
            pickerError = CallInviteOfferRules.sendFailedMessage
            return false
        }
        invited.insert(target.id)
        show(.ready(url: url, delivery: .sent(name: target.title)))
        return true
    }

    /// The link to this call, minted on first use.
    ///
    /// `nil` when there is none to give: a mint is already in flight, or the
    /// mint failed — and then the status line already says so.
    private func linkForCall() async -> String? {
        switch CallInviteOfferRules.nextPress(state) {
        case .ignore:
            return nil
        case .reoffer:
            if let url = CallInviteOfferRules.mintedUrl(state) { return url }
            return nil
        case .mint:
            break
        }
        show(.minting)
        do {
            return try await mint()
        } catch {
            NSLog("call invite: mint failed: %@", String(describing: error))
            show(.failed(failure: CallInviteOfferRules.failure(for: error), url: nil))
            return nil
        }
    }

    /// Move to `next` and put its line on the screen for as long as it holds.
    private func show(_ next: CallInviteOfferState) {
        holdTask?.cancel()
        holdTask = nil
        state = next
        lineShown = CallInviteOfferRules.statusLine(next) != nil
        guard let ms = CallInviteOfferRules.statusHoldMs(next) else { return }
        let hold = self.hold
        holdTask = Task { @MainActor [weak self] in
            await hold(ms)
            guard !Task.isCancelled else { return }
            self?.lineShown = false
        }
    }
}

/// Why an offer built without a send path refuses to send.
public enum CallInviteOfferError: Error {
    case noSendPath
}
