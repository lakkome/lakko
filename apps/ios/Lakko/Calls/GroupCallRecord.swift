// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// MARK: - Group-call signals

/// The group-call control bodies of `CallSignalingPayload`
/// (`schemas/mls-app/call_signaling_v1.proto` fields 14..16, plus the reused
/// `CallEnd` as the group terminal), lifted off the FFI enum into a plain
/// value so the record reducer below stays FFI-free and unit-testable.
///
/// The wire is unchanged — this is only the in-app shape of bodies
/// `composeCallSignaling` / `parseCallSignaling` already carry.
enum GroupCallSignal: Equatable {
    /// `GroupCallStart` — the initiator announces a call id to the chat.
    case start(callId: Data, initiatorLeaf: UInt32, createdAtMs: UInt64, audio: Bool, video: Bool)
    /// `GroupCallJoin` — a member connected to the SFU room for `callId`.
    case join(callId: Data, leafIndex: UInt32)
    /// `GroupCallLeave` — a member left the SFU room for `callId`.
    case leave(callId: Data, leafIndex: UInt32)
    /// `CallEnd` reused as the group terminal for `callId`.
    case end(callId: Data)

    /// The leaf a body claims to speak for — the initiator of a start, the
    /// joiner or leaver of a join / leave. `nil` for `CallEnd`, which names no
    /// leaf (and whose effect is bounded by C-12's `call_id` match instead).
    var claimedLeaf: UInt32? {
        switch self {
        case .start(_, let initiatorLeaf, _, _, _): return initiatorLeaf
        case .join(_, let leafIndex), .leave(_, let leafIndex): return leafIndex
        case .end: return nil
        }
    }

    /// A body may only speak for its own sender.
    ///
    /// The leaf a `GroupCallStart` / `Join` / `Leave` names is an
    /// unauthenticated field inside the payload; the MLS sender leaf of the
    /// journal row that carried it is not. A body naming any leaf but its
    /// sender's — or arriving on a row with no member sender at all — is
    /// refused rather than trusted, so one member cannot announce a call as
    /// someone else, join someone else into the roster, or leave them out of
    /// it. Same rule as the web client; no wire change.
    ///
    /// `CallEnd` claims no leaf and passes: any member may end a call id it
    /// can name, exactly as before.
    func speaksFor(senderLeaf: UInt32?) -> Bool {
        guard let claimed = claimedLeaf else { return true }
        guard let senderLeaf else { return false }
        return claimed == senderLeaf
    }
}

/// Where one parsed call-signaling body is allowed to go.
///
/// This is the C-11 boundary written down once: a group-control body is
/// NEVER forwarded to the 1x1 `CallSession`. `CallEnd` is the one body that is
/// meaningful to both — it is the 1x1 terminal and the group terminal — and it
/// is safe to hand to both because each consumer drops a `call_id` that is not
/// its own (C-12).
struct CallSignalingRoute: Equatable {
    /// The group-call record update this body carries, if any.
    let groupSignal: GroupCallSignal?
    /// `true` when the body may reach the 1x1 `CallSession` dispatcher.
    let forwardToOneToOne: Bool

    static func of(_ body: FfiCallSignalingBody) -> CallSignalingRoute {
        switch body {
        case .offer, .answer, .iceCandidate:
            return CallSignalingRoute(groupSignal: nil, forwardToOneToOne: true)
        case .end(let callId, _):
            return CallSignalingRoute(groupSignal: .end(callId: callId), forwardToOneToOne: true)
        case .groupStart(let callId, let initiatorLeaf, let createdAtMs, let media):
            return CallSignalingRoute(
                groupSignal: .start(
                    callId: callId,
                    initiatorLeaf: initiatorLeaf,
                    createdAtMs: createdAtMs,
                    audio: media.audio,
                    video: media.video
                ),
                forwardToOneToOne: false
            )
        case .groupJoin(let callId, let leafIndex, _):
            return CallSignalingRoute(
                groupSignal: .join(callId: callId, leafIndex: leafIndex),
                forwardToOneToOne: false
            )
        case .groupLeave(let callId, let leafIndex, _):
            return CallSignalingRoute(
                groupSignal: .leave(callId: callId, leafIndex: leafIndex),
                forwardToOneToOne: false
            )
        }
    }
}

// MARK: - The record

/// One chat's ongoing group call, as this device has observed it on the group
/// journal.
///
/// Advisory state for the "Group call in progress" banner, not an
/// authorisation: joining still needs the MLS exporter seed (only a current
/// member derives it) and a call-admission JWT, so a wrong record can at worst
/// show or hide a banner.
struct OngoingGroupCall: Equatable {
    let callId: Data
    let audio: Bool
    let video: Bool
    let initiatorLeaf: UInt32
    /// The announce's `created_at_ms`, bounded above by the DS row's
    /// server-assigned `created_at` — a sender cannot keep its call "fresh" by
    /// claiming a future start.
    let startedAtMs: Int64
    /// Leaves that announced a join and have not announced a leave.
    var joinedLeaves: Set<UInt32>
    /// Newest DS row time that touched this record.
    var lastActivityMs: Int64

    /// Lowercase hex of ``callId`` — the tie-break key and the JWT `call_id`.
    var callIdHex: String {
        callId.map { String(format: "%02x", $0) }.joined()
    }
}

/// Pure reducer + liveness rules for ``OngoingGroupCall``. The same contract
/// the web and Android clients implement; nothing here reads a clock of its
/// own, so the tests drive every branch with explicit times.
enum GroupCallReducer {

    /// A record older than this is never live, joined leaves or not — a
    /// member that crashed without a `GroupCallLeave` must not pin a banner
    /// for ever.
    static let maxLiveAgeMs: Int64 = 4 * 60 * 60 * 1000

    /// How long a call with nobody joined yet still counts as live — the
    /// window between the initiator's announce and its own join landing.
    static let unjoinedGraceMs: Int64 = 60 * 1000

    /// LIVE = started within the last 4 h AND (somebody is joined OR it was
    /// started within the last 60 s). A start stamped slightly in the future
    /// of the local clock (server/device skew) reads as age 0.
    static func isLive(_ call: OngoingGroupCall, nowMs: Int64) -> Bool {
        let age = max(0, nowMs &- call.startedAtMs)
        guard age <= maxLiveAgeMs else { return false }
        return !call.joinedLeaves.isEmpty || age <= unjoinedGraceMs
    }

    /// Fold one signal into the chat's record.
    ///
    /// - Parameters:
    ///   - rowCreatedAtMs: the DS row's server-assigned time (or, for this
    ///     device's own signals, which are never echoed back, the local time of
    ///     the publish). Also the instant at which a competing start is judged,
    ///     so two devices replaying the same journal reach the same record.
    static func apply(
        _ signal: GroupCallSignal,
        to current: OngoingGroupCall?,
        rowCreatedAtMs: Int64
    ) -> OngoingGroupCall? {
        switch signal {
        case .start(let callId, let initiatorLeaf, let createdAtMs, let audio, let video):
            let claimed = Int64(clamping: createdAtMs)
            let candidate = OngoingGroupCall(
                callId: callId,
                audio: audio,
                video: video,
                initiatorLeaf: initiatorLeaf,
                startedAtMs: min(claimed, rowCreatedAtMs),
                joinedLeaves: [],
                lastActivityMs: rowCreatedAtMs
            )
            guard var existing = current else { return candidate }
            if existing.callId == callId {
                // A re-announce of the call we already track: keep what we know
                // (its joins in particular), only note the activity.
                existing.lastActivityMs = max(existing.lastActivityMs, rowCreatedAtMs)
                return existing
            }
            let existingLive = isLive(existing, nowMs: rowCreatedAtMs)
            if !existingLive { return candidate }
            let candidateLive = isLive(candidate, nowMs: rowCreatedAtMs)
            if !candidateLive { return existing }
            // Two different live call ids — the lowest (start, call_id hex)
            // wins, so every member converges on one call.
            return precedes(candidate, existing) ? candidate : existing

        case .join(let callId, let leafIndex):
            // C-12 — a join for a call id we do not track is dropped.
            guard var existing = current, existing.callId == callId else { return current }
            existing.joinedLeaves.insert(leafIndex)
            existing.lastActivityMs = max(existing.lastActivityMs, rowCreatedAtMs)
            return existing

        case .leave(let callId, let leafIndex):
            guard var existing = current, existing.callId == callId else { return current }
            existing.joinedLeaves.remove(leafIndex)
            existing.lastActivityMs = max(existing.lastActivityMs, rowCreatedAtMs)
            return existing

        case .end(let callId):
            guard let existing = current, existing.callId == callId else { return current }
            return nil
        }
    }

    /// The tie-break order: earlier bounded start first, then lower call id hex.
    static func precedes(_ a: OngoingGroupCall, _ b: OngoingGroupCall) -> Bool {
        if a.startedAtMs != b.startedAtMs { return a.startedAtMs < b.startedAtMs }
        return a.callIdHex < b.callIdHex
    }
}

// MARK: - What the chat screen shows

/// The banner / toolbar decision for one chat, derived from its record.
enum GroupCallAffordance: Equatable {
    /// No live call this device could join — the call buttons start a new one.
    case none
    /// A live call exists and this device is not in it: show the banner, and
    /// the call buttons join this call instead of starting another.
    case joinable(callId: Data, joinedCount: Int, video: Bool)
    /// This device is already in the live call.
    case inCall

    static func of(
        record: OngoingGroupCall?,
        activeGroupCallId: Data?,
        nowMs: Int64
    ) -> GroupCallAffordance {
        guard let record, GroupCallReducer.isLive(record, nowMs: nowMs) else {
            return activeGroupCallId == nil ? .none : .inCall
        }
        if activeGroupCallId == record.callId { return .inCall }
        return .joinable(
            callId: record.callId,
            joinedCount: record.joinedLeaves.count,
            video: record.video
        )
    }
}
