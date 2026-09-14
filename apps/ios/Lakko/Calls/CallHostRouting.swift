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
///     ringing, being prepared, live, or while a group (SFU) call is running is
///     answered `End{busy}` so the caller stops ringing, never silently dropped.
///   * **Stale offers do not ring.** An offer whose DS row is older than
///     ``staleOfferMs`` does not ring and is not answered busy either: the
///     caller gave up long ago and nobody is waiting on a reply.
///   * **An ended call never rings again**, whatever the journal still holds.
///
/// Same contract as `apps/web/src/calls/call-host-routing.ts`.
enum CallHostRouting {

    /// An offer older than this, by its DS row timestamp, does not ring.
    static let staleOfferMs: Int64 = 60_000

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
        /// Publish `End{busy}` to the offering group; do not ring.
        case busy
        /// Hand the body to the session in the slot.
        case apply
    }

    /// Is an offer too old to ring for?
    ///
    /// Judged on the DS row's server timestamp, never on anything the caller
    /// stamped into the payload. An unreadable timestamp is treated as stale: a
    /// call whose age cannot be established does not ring.
    static func isStaleOffer(createdAt: String, nowMs: Int64, maxAgeMs: Int64 = staleOfferMs) -> Bool {
        guard let rowMs = ChatRepository.parseDsTimestampMs(createdAt) else { return true }
        return nowMs - rowMs > maxAgeMs
    }

    /// Decide one inbound row. Pure.
    ///
    /// - Parameters:
    ///   - active: the call occupying the slot, or `nil` when the slot is free
    ///     (no session, or a session that has reached `.done`).
    ///   - endedCallIds: hex call ids of 1x1 calls that ended on this device
    ///     this session.
    ///   - groupCallBusy: a group (SFU) call is starting or live on this device.
    @MainActor
    static func decide(
        row: InboundCallRow,
        nowMs: Int64,
        active: ActiveCall?,
        endedCallIds: Set<String>,
        groupCallBusy: Bool
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
            if isStaleOffer(createdAt: row.createdAt, nowMs: nowMs) { return .drop }
            if active != nil || groupCallBusy { return .busy }
            return .ring
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
/// Chosen, not measured — the same values as the web client's
/// `DEFAULT_CALL_SESSION_TIMING`.
struct CallHostTiming: Equatable {
    /// An incoming call rings for this long.
    var ringTimeoutMs: Int64 = 45_000
    /// From dial (or accept) until the transport connects.
    var establishTimeoutMs: Int64 = 45_000
}
