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

/// Where the link to this call is.
public enum CallInviteOfferState: Equatable {
    /// Nobody has asked for a link yet, so none has been made. This is the
    /// state every call starts in, and the reason starting a call is instant.
    case idle
    /// Being made. A round trip and an anti-spam check, so it is not instant
    /// and the screen has to say what it is doing.
    case minting
    /// Made. Every later ask re-offers this one.
    case ready(url: String)
    /// The last attempt failed. The call is untouched; asking again retries.
    case failed(message: String)
}

/// What one press of `Invite` does, and what the screen says while it happens.
///
/// Pure, because a SwiftUI body is the one place in this app a test cannot
/// reach — the same reason ``CallScreenPresentation`` exists.
public enum CallInviteOfferRules {

    /// The one thing a press does.
    public enum Action: Equatable {
        /// No link yet: make one.
        case mint
        /// There is one already: offer it again, without a second round trip.
        case offer(url: String)
        /// One is being made right now. A second press must not start a second
        /// mint — the link would be a different link for the same call.
        case ignore
    }

    public static func press(_ state: CallInviteOfferState) -> Action {
        switch state {
        case .idle, .failed:
            return .mint
        case .minting:
            return .ignore
        case .ready(let url):
            return .offer(url: url)
        }
    }

    /// The word under the control. It names the act, never the state, so it
    /// does not change size or meaning when the state flips.
    public static var controlLabel: String { Localization.text("call.control.invite") }

    /// What the busy state says. The spec is explicit that it says what it is
    /// doing rather than only spinning: a spinner for a few seconds with no
    /// words reads as a stuck screen.
    public static var mintingMessage: String { Localization.text("call.invite.minting") }

    /// What a ready link says about itself. Both sentences are load-bearing:
    /// the first is what the link is for, the second is why no setting was
    /// asked for — the person admits each arrival, so the link lets someone
    /// ask and never lets them in.
    /// The link as a person should read it: without the scheme.
    ///
    /// A person about to send a link should see WHICH link, and the panel that
    /// showed only a `Copy` button beside the words "Call link" was asking them
    /// to send something they had never seen. The scheme comes off because it
    /// is the same on every one of them and reads as noise.
    ///
    /// **It does not shorten.** Shortening here and letting the layout
    /// truncate as well elided the same link twice - `test.lakko.me/c...5j...`
    /// - which reads as a broken string rather than as a shortened one. How
    /// much fits is a question about the width of a particular screen and a
    /// particular type size, so the view answers it; this only removes what is
    /// noise on every screen.
    ///
    /// Display only. It is never what gets copied or shared.
    @MainActor
    public static func shortLink(_ url: String) -> String {
        var text = url
        for scheme in ["https://", "http://"] where text.hasPrefix(scheme) {
            text = String(text.dropFirst(scheme.count))
        }
        return text
    }

    public static var readyMessage: String { Localization.text("call.invite.ready") }

    /// One sentence on what happened, one on what to do. Never ends the call,
    /// and says so, because a failure inside a live call reads as the call
    /// having broken when nothing has. Rendered from the kind of failure; the
    /// failure's own prose goes to the log only (ADR-0061).
    public static func failureMessage(for error: Error) -> String {
        if case LinkCallHostError.signedOut = error { return signedOutMessage }
        return Localization.text("call.invite.unavailable")
    }

    /// What a signed-out device says. Section 5's last row: one sentence.
    public static var signedOutMessage: String { Localization.text("call.invite.signed_out") }
}

// MARK: - The offer

/// The link to one live call, minted the first time it is asked for.
///
/// Owned by ``LinkCallHost`` for the duration of a call this device founded, so
/// the second `Invite` in a call finds the link the first one made, and a new
/// call starts with no link at all.
///
/// Only the call's founder holds one. A capability names its signer as the
/// call's owner, and ADR-0032 elects the committer from that owner, so a link
/// minted by anyone else would send arrivals to a call nobody is founding.
@MainActor
public final class CallInviteOffer: ObservableObject {

    @Published public private(set) var state: CallInviteOfferState = .idle
    /// The panel that shows the state and hands the link to the share sheet.
    @Published public private(set) var panelOpen = false

    private let mint: @MainActor () async throws -> String

    /// - Parameter mint: makes the link for THIS call and returns its URL.
    public init(mint: @MainActor @escaping () async throws -> String) {
        self.mint = mint
    }

    /// The URL, once there is one.
    public var url: String? {
        if case .ready(let url) = state { return url }
        return nil
    }

    /// `Invite` was pressed.
    ///
    /// Opens the panel first and in every case, so the busy and failed states
    /// are seen rather than happening off-screen; the mint runs underneath it.
    public func press() async {
        panelOpen = true
        switch CallInviteOfferRules.press(state) {
        case .ignore, .offer:
            return
        case .mint:
            await mintNow()
        }
    }

    /// Retry after a failure, from the panel.
    public func retry() async {
        guard case .failed = state else { return }
        await mintNow()
    }

    public func setPanelOpen(_ open: Bool) {
        panelOpen = open
    }

    private func mintNow() async {
        state = .minting
        do {
            let url = try await mint()
            state = .ready(url: url)
        } catch {
            NSLog("call invite: mint failed: %@", String(describing: error))
            state = .failed(message: CallInviteOfferRules.failureMessage(for: error))
        }
    }
}
