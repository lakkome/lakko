// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// One decrypted 1x1-eligible call-signaling row, as the receive path hands it
/// to the app-level ``CallHost``.
///
/// Built only by ``ChatRepository/routeCallSignaling(groupIdHex:messageId:payload:createdAt:senderLeaf:)``,
/// and only for a body ``CallSignalingRoute`` forwards to the 1x1 side (offer /
/// answer / ICE candidate / `CallEnd`). A group-control body is folded into the
/// group-call record there and never becomes one of these (C-11).
struct InboundCallRow: Equatable {
    /// The group whose journal carried the row.
    let groupIdHex: String
    /// The DS journal id. Unique only WITHIN one group's journal, so the
    /// exactly-once key is the pair (``groupIdHex``, ``messageId``).
    let messageId: Int64
    /// The DS row's server-assigned `created_at`, the only clock the stale
    /// rule reads.
    let createdAt: String
    let body: FfiCallSignalingBody

    /// The exactly-once key. Row 42 of one group is not row 42 of another.
    var dedupKey: String { "\(groupIdHex)#\(messageId)" }
}

/// Where one inbound row goes once the 1x1 call lives at the app level rather
/// than inside one chat's view model.
///
/// ## Why the decision moved
///
/// The 1x1 `CallSession` used to be owned by `ChatViewModel`, fed through the
/// repository's single `callSignalingSink` slot that the most recently built
/// view model claimed and filtered to its own group. An offer to any chat that
/// was not on screen was dropped at that filter; leaving the chat dropped the
/// view model and the call with it. The call now lives in ``CallHost``, created
/// once per process, fed from every receive path.
///
/// Folding many groups into one call slot adds questions a per-chat view never
/// had to ask. They are answered here, in a pure function, so every branch is
/// testable without WebRTC:
///
///   * **C-12, widened.** An answer / ICE candidate / `CallEnd` must name the
///     active call AND arrive on the group that call lives in. The same 16-byte
///     call id on a different group's journal is not this call.
///   * **One call at a time.** A fresh offer for a different call while one is
///     ringing, being prepared, live, or while a group (SFU) or call-link call
///     holds the app's one call slot (``CallSlot``) is answered `End{busy}` so
///     the caller stops ringing, never silently dropped.
///   * **Stale offers do not ring.** An offer older than the ring window by its
///     DS row time does not ring and is not answered busy either: the caller
///     gave up already and nobody is waiting on a reply.
///   * **A muted conversation never rings** (`docs/ux/calls.md` section 2.1).
///     It is still recorded — only the interruption is withheld.
///   * **An ended call never rings again**, whatever the journal still holds.
///
/// The last two are one question, answered once for all three clients by
/// `LakkoCore.ringDecision(muted:offerAgeMs:)`, so this file states neither
/// the window nor the mute rule itself.
///
/// Same contract as `apps/web/src/calls/call-host-routing.ts`.
enum CallHostRouting {

    /// How long an offer may ring for, by its DS row timestamp.
    ///
    /// Read from ``LakkoCore/callRingWindowMs()`` rather than written here: it
    /// is a number the CALLER'S device has to agree with, and two copies means
    /// one side files a missed call while the other is still ringing. This used
    /// to be a local 60 s grace that outlived the 45 s the caller waits, so an
    /// offer between the two rang here for a call nobody was on the other end
    /// of.
    static var ringWindowMs: UInt64 { LakkoCore.callRingWindowMs() }

    /// The 1x1 call currently occupying the slot, as routing needs to see it.
    struct ActiveCall: Equatable {
        let callIdHex: String
        let groupIdHex: String
    }

    /// What happens on the 1x1 side for one row.
    enum Action: Equatable {
        /// Nothing — not addressed to the call in the slot, stale, or ended.
        case drop
        /// Open a ringing answerer session for this offer.
        case ring
        /// Record the call; interrupt nobody. The conversation is muted.
        ///
        /// Deliberately NOT ``drop``. A person who muted a conversation asked
        /// not to be interrupted by it; they did not ask to be kept from
        /// finding out who tried to reach them, so the call still lands in the
        /// history and the Calls list exactly as an unanswered unmuted one
        /// does. Only the ringtone, the vibration and the screen that takes
        /// over the phone are withheld.
        case silent
        /// Publish `End{busy}` to the offering group; do not ring.
        case busy
        /// Hand the body to the session in the slot.
        case apply
    }

    /// How old an offer is, in milliseconds, or `nil` when its row time cannot
    /// be read.
    ///
    /// Judged on the DS row's server timestamp, never on anything the caller
    /// stamped into the payload — the two devices share no clock, and a caller
    /// that stamped its own offer could keep it ringing forever. A `nil` means
    /// the call does not ring: an age that cannot be established is not an age
    /// the window can be measured against.
    static func offerAgeMs(createdAt: String, nowMs: Int64) -> UInt64? {
        DsTimestamp.ageMs(createdAt: createdAt, nowMs: nowMs)
    }

    /// Decide one inbound row. Pure.
    ///
    /// - Parameters:
    ///   - active: the call occupying the slot, or `nil` when the slot is free
    ///     (no session, or a session that has reached `.done`).
    ///   - endedCallIds: hex call ids of 1x1 calls that ended on this device
    ///     this session.
    ///   - otherCallBusy: a group (SFU) or call-link call is starting, waiting or
    ///     live on this device.
    ///   - muted: the conversation carries the mute flag. Defaulted `false` so
    ///     a caller that cannot answer the question rings rather than goes
    ///     quiet — the direction a missing answer has to fail in, since an
    ///     unwanted ring is an annoyance and a swallowed call is a call the
    ///     person never learns about.
    @MainActor
    static func decide(
        row: InboundCallRow,
        nowMs: Int64,
        active: ActiveCall?,
        endedCallIds: Set<String>,
        otherCallBusy: Bool,
        muted: Bool = false
    ) -> Action {
        let callIdHex = hex(CallSession.callId(of: row.body))
        let forActive = active.map {
            $0.callIdHex == callIdHex && $0.groupIdHex == row.groupIdHex
        } ?? false
        switch row.body {
        case .groupStart, .groupJoin, .groupLeave:
            // C-11 — never reaches the 1x1 side, even if a caller mis-routes.
            return .drop
        case .answer, .iceCandidate, .end:
            return forActive ? .apply : .drop
        case .offer:
            // The call in the slot: a renegotiation offer (camera turned on).
            if forActive { return .apply }
            if endedCallIds.contains(callIdHex) { return .drop }
            guard let ageMs = offerAgeMs(createdAt: row.createdAt, nowMs: nowMs) else {
                return .drop
            }
            let verdict = LakkoCore.ringDecision(muted: muted, offerAgeMs: ageMs)
            if verdict == .tooLate { return .drop }
            // Busy is decided ABOVE mute, and the order matters. `End{busy}` is
            // a message to the CALLER, not an interruption of the person here:
            // it stops a phone ringing in someone else's hand. Withholding it
            // because this conversation is muted would make the caller wait out
            // the whole window for no reason.
            if active != nil || otherCallBusy { return .busy }
            return verdict == .silent ? .silent : .ring
        }
    }

    static func hex(_ data: Data) -> String {
        data.map { String(format: "%02x", $0) }.joined()
    }
}

/// Timers bounding how long a 1x1 call may sit in a non-terminal state.
///
/// Without them a call nobody answers never ends: the caller shows "Calling"
/// forever and the callee rings until the app is killed. Each expiry ends the
/// session locally, which publishes `End{timedOut}` so the other side stops too.
/// The ring half is the shared window; only ``establishTimeoutMs`` is a local
/// choice (the same value as the web client's `DEFAULT_CALL_SESSION_TIMING`).
struct CallHostTiming: Equatable {
    /// An incoming call rings for this long. The shared window
    /// (``CallHostRouting/ringWindowMs``), never a second copy of the number.
    var ringTimeoutMs: Int64 = Int64(CallHostRouting.ringWindowMs)
    /// From dial (or accept) until the transport connects.
    var establishTimeoutMs: Int64 = 45_000
}
