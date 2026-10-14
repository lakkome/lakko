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

/// Who a group-call join / leave row came from, resolved from the MLS sender
/// leaf AT RECEIPT (C-68) — never a leaf index, which OpenMLS hands to the next
/// member added after a remove.
struct GroupCallParticipant: Equatable {
    /// The leaf's own signature key (base64): one DEVICE. Membership in the call
    /// is keyed on this, so a Leave from one device never removes another
    /// device of the same person.
    let deviceKey: MemberIdentity
    /// The ADR-0050 person (account) key (base64) of that leaf, for counting
    /// people rather than devices.
    let personKey: MemberIdentity
}

/// One device's place in a call: when it was last seen and whose device it is.
struct JoinedDevice: Equatable, Codable {
    /// DS row time of this device's newest join / heartbeat.
    var lastSeenMs: Int64
    /// The person (account) key the device belongs to.
    let personKey: MemberIdentity
}

/// One chat's ongoing group call, as this device has observed it on the group
/// journal.
///
/// Advisory state for the "Group call in progress" banner, not an
/// authorisation: joining still needs the MLS exporter seed (only a current
/// member derives it) and a call-admission JWT, so a wrong record can at worst
/// show or hide a banner.
///
/// Codable because it is persisted per account (``GroupCallStore``): the DS
/// poll resumes from its cursor, so a record held only in memory was forgotten
/// on relaunch and the chat offered to start a second call beside a running one.
struct OngoingGroupCall: Equatable, Codable {
    let callId: Data
    /// The announced media. Meaningful only when ``initiatorLeaf`` is non-nil —
    /// a record adopted from a Join never saw the announce and carries `false`
    /// for "unknown", not for "voice". Nothing reads these today; anything that
    /// starts to must consult ``mediaKnown`` first.
    let audio: Bool
    let video: Bool
    /// The leaf that announced the call, or `nil` when this record was ADOPTED
    /// from a Join because this device never received the announce.
    ///
    /// The Start is published exactly once, by the initiator, before it
    /// connects (`GroupCallHost`); there is no re-announce. A device that was
    /// not polling in that moment — asleep, backgrounded, launched later —
    /// therefore never sees it, and before adoption existed every subsequent
    /// heartbeat from the people actually in that call was dropped, leaving the
    /// chat with no idea a call was running.
    let initiatorLeaf: UInt32?
    /// The person (account key, base64) whose device announced the call,
    /// resolved from the MLS sender of the `GroupCallStart` at receipt (C-68) —
    /// never the leaf the body names. For a call this device started, its own
    /// person key. Only this person may end the call for everyone
    /// (`docs/ux/calls.md` section 4.1.2).
    ///
    /// `nil` is "unknown": the sender did not resolve, the record was adopted
    /// from a Join, or it was stored before this field existed. A call whose
    /// starter is unknown can never be ended for everyone.
    var initiatorPersonKey: MemberIdentity? = nil
    /// The announce's `created_at_ms`, bounded above by the DS row's
    /// server-assigned `created_at` — a sender cannot keep its call "fresh" by
    /// claiming a future start.
    let startedAtMs: Int64
    /// Devices that announced a join (or a heartbeat re-join) and have not
    /// announced a leave, keyed on the device's own leaf signature key resolved
    /// at receipt (``GroupCallParticipant``).
    ///
    /// A device only counts while its last-seen time is fresh
    /// (``GroupCallReducer/memberFreshMs``): a participant whose app was killed
    /// never publishes its Leave, and its entry must stop keeping the call alive
    /// once its heartbeats stop.
    var joined: [MemberIdentity: JoinedDevice]
    /// Newest DS row time that touched this record.
    var lastActivityMs: Int64

    /// This record came from the call's own announce, so ``audio`` / ``video``
    /// describe the call rather than standing in for the unknown.
    var mediaKnown: Bool { initiatorLeaf != nil }

    /// Lowercase hex of ``callId`` — the tie-break key and the JWT `call_id`.
    var callIdHex: String {
        callId.map { String(format: "%02x", $0) }.joined()
    }

    /// The joined devices whose newest join / heartbeat is fresh at `nowMs`,
    /// minus `ignoringDevice`. A time slightly in the future of the local clock
    /// (server/device skew) is fresh.
    ///
    /// `ignoringDevice` is this device's own key when it is NOT in the call: an
    /// entry it left behind before a relaunch must not make the call look live,
    /// or be counted, to the device that wrote it.
    func freshDevices(nowMs: Int64, ignoringDevice: MemberIdentity? = nil) -> Set<MemberIdentity> {
        Set(joined.compactMap { device, entry in
            guard device != ignoringDevice,
                  nowMs &- entry.lastSeenMs <= GroupCallReducer.memberFreshMs else { return nil }
            return device
        })
    }

    /// The newest last-seen time among the joined devices, fresh or not.
    ///
    /// ``freshDevices`` answers "is anyone here"; this answers "how far off were
    /// we", which is the number a log needs when the answer is no. A record that
    /// is judged stale because of clock skew and one that is judged stale
    /// because everybody really did leave are the same verdict and completely
    /// different bugs, and only the age tells them apart.
    func newestLastSeenMs(ignoringDevice: MemberIdentity? = nil) -> Int64? {
        joined.lazy
            .filter { $0.key != ignoringDevice }
            .map(\.value.lastSeenMs)
            .max()
    }

    /// The number of distinct PEOPLE among the fresh devices — what "N people in
    /// the call" shows. Two devices of one person count once.
    func freshPeopleCount(nowMs: Int64, ignoringDevice: MemberIdentity? = nil) -> Int {
        Set(freshDevices(nowMs: nowMs, ignoringDevice: ignoringDevice)
            .compactMap { joined[$0]?.personKey }).count
    }
}

/// Pure reducer + liveness rules for ``OngoingGroupCall``. The same contract
/// the web and Android clients implement; nothing here reads a clock of its
/// own, so the tests drive every branch with explicit times.
enum GroupCallReducer {

    /// A record older than this is never live, joined members or not.
    static let maxLiveAgeMs: Int64 = 4 * 60 * 60 * 1000

    /// How long a call with nobody joined yet still counts as live — the
    /// window between the initiator's announce and its own join landing.
    static let unjoinedGraceMs: Int64 = 60 * 1000

    /// A device in a group call re-publishes its own `GroupCallJoin` this often
    /// (`groupCallHeartbeatMs`), for as long as it is in the call.
    static let heartbeatMs: Int64 = 60 * 1000

    /// A joined member counts only while its newest join / heartbeat is at most
    /// this old (`groupCallMemberFreshMs`): two heartbeat intervals plus a poll's
    /// worth of slack, so one lost heartbeat does not drop a member.
    static let memberFreshMs: Int64 = 150 * 1000

    /// LIVE = started within the last 4 h AND (some device other than
    /// `ignoringDevice` is FRESH OR it was started within the last 60 s). A
    /// start stamped slightly in the future of the local clock (server/device
    /// skew) reads as age 0.
    static func isLive(
        _ call: OngoingGroupCall, nowMs: Int64, ignoringDevice: MemberIdentity? = nil
    ) -> Bool {
        let age = max(0, nowMs &- call.startedAtMs)
        guard age <= maxLiveAgeMs else { return false }
        return !call.freshDevices(nowMs: nowMs, ignoringDevice: ignoringDevice).isEmpty
            || age <= unjoinedGraceMs
    }

    /// Fold one signal into the chat's record.
    ///
    /// - Parameters:
    ///   - participant: the row's MLS sender resolved at receipt. A join or
    ///     leave with no resolvable sender is dropped; a start and an end do not
    ///     need one. A start records its sender's person as the call's starter
    ///     (``OngoingGroupCall/initiatorPersonKey``), unknown without one.
    ///   - rowCreatedAtMs: the DS row's server-assigned time (or, for this
    ///     device's own signals, which are never echoed back, the local time of
    ///     the publish). Also the instant at which a competing start is judged,
    ///     so two devices replaying the same journal reach the same record.
    static func apply(
        _ signal: GroupCallSignal,
        participant: GroupCallParticipant?,
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
                initiatorPersonKey: participant?.personKey,
                startedAtMs: min(claimed, rowCreatedAtMs),
                joined: [:],
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
            guard let participant else { return current }
            guard var existing = current, existing.callId == callId else {
                return adopt(
                    callId: callId, joiningLeaf: leafIndex, participant: participant,
                    displacing: current, rowCreatedAtMs: rowCreatedAtMs)
            }
            // A join or heartbeat advances the device's last-seen time; an older
            // row never lowers it.
            let lastSeen = max(existing.joined[participant.deviceKey]?.lastSeenMs ?? rowCreatedAtMs, rowCreatedAtMs)
            existing.joined[participant.deviceKey] = JoinedDevice(
                lastSeenMs: lastSeen, personKey: participant.personKey)
            existing.lastActivityMs = max(existing.lastActivityMs, rowCreatedAtMs)
            return existing

        case .leave(let callId, _):
            guard var existing = current, existing.callId == callId else {
                // A leave for some other call says nothing about the one we
                // hold — unless the one we hold is already dead, in which case
                // this is as good a moment as any to stop carrying it.
                if let stale = current, !isLive(stale, nowMs: rowCreatedAtMs) { return nil }
                return current
            }
            guard let participant else { return current }
            // Only that device leaves; another device of the same person stays.
            existing.joined[participant.deviceKey] = nil
            existing.lastActivityMs = max(existing.lastActivityMs, rowCreatedAtMs)
            return existing

        case .end(let callId):
            guard let existing = current, existing.callId == callId else { return current }
            return nil
        }
    }

    /// A Join naming a call this device does not hold.
    ///
    /// ## Why this is not simply dropped
    ///
    /// It used to be, and that is the defect this rule exists to close. One
    /// chat holds exactly ONE call record. A record for a call that ended is
    /// pruned only by the four-hour age cap, so a dead call sat in the slot for
    /// hours — and because a Join whose id did not match was returned
    /// unchanged, every heartbeat from the people in the REAL call folded to a
    /// no-op: no state change, therefore no publish, therefore not even a
    /// recomputed verdict. A live group call was invisible in the chat for as
    /// long as the corpse of the previous one outranked it, and the only thing
    /// the screen could report was that the old call was not live, which was
    /// true and useless.
    ///
    /// ## What is preserved
    ///
    /// The protective half of the rule the old comment cited (C-12) is intact:
    /// a **live** record is never displaced by a foreign call id, so a stranger
    /// in the chat cannot steal the slot from a call in progress by publishing
    /// a fabricated one. Only a record that is already not live gives way. The
    /// invariant as written in `docs/security/calls.md` is about a live
    /// `GroupCallSession`'s roster, which this advisory record is not, and the
    /// comment here previously blurred the two.
    ///
    /// ## What is honestly unknown
    ///
    /// The announce is published once and never repeated, so a device that
    /// missed it cannot recover the call's start time, its media, or who began
    /// it. The adopted record says so: ``OngoingGroupCall/initiatorLeaf`` is
    /// nil, ``OngoingGroupCall/mediaKnown`` is false, and the start is the DS
    /// row's own time — the earliest moment this device can prove the call was
    /// running, never a guess at when it began. Since the start time is the
    /// only thing the unjoined grace reads, an adopted record cannot claim that
    /// grace on a call that started long ago: it is live because somebody is
    /// demonstrably in it, which is the stronger of the two reasons anyway.
    private static func adopt(
        callId: Data,
        joiningLeaf: UInt32,
        participant: GroupCallParticipant,
        displacing current: OngoingGroupCall?,
        rowCreatedAtMs: Int64
    ) -> OngoingGroupCall? {
        if let current, isLive(current, nowMs: rowCreatedAtMs) { return current }
        return OngoingGroupCall(
            callId: callId,
            audio: true,
            video: false,
            initiatorLeaf: nil,
            startedAtMs: rowCreatedAtMs,
            joined: [
                participant.deviceKey: JoinedDevice(
                    lastSeenMs: rowCreatedAtMs, personKey: participant.personKey)
            ],
            lastActivityMs: rowCreatedAtMs
        )
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

    /// - Parameter ownDeviceKey: this device's own leaf signature key in the
    ///   group. While this device is not in the call, its own entry (left behind
    ///   by a relaunch mid-call) is ignored in liveness and in the count.
    static func of(
        record: OngoingGroupCall?,
        activeGroupCallId: Data?,
        ownDeviceKey: MemberIdentity?,
        nowMs: Int64
    ) -> GroupCallAffordance {
        if let record, activeGroupCallId == record.callId,
           GroupCallReducer.isLive(record, nowMs: nowMs) {
            return .inCall
        }
        guard let record,
              GroupCallReducer.isLive(record, nowMs: nowMs, ignoringDevice: ownDeviceKey) else {
            return activeGroupCallId == nil ? .none : .inCall
        }
        if activeGroupCallId == record.callId { return .inCall }
        return .joinable(
            callId: record.callId,
            joinedCount: record.freshPeopleCount(nowMs: nowMs, ignoringDevice: ownDeviceKey),
            video: record.video
        )
    }
}

/// Names for the group-call signals in log lines.
///
/// A kind and an eight-hex-character call-id prefix are enough to line an iOS
/// log up against the SFU's own — which prints the same prefix — and they carry
/// nothing else: a leaf index, a device key or a person key would be identity,
/// and identity does not belong in a log.
enum GroupCallSignalLog {

    static func kind(_ signal: GroupCallSignal) -> String {
        switch signal {
        case .start(let callId, _, _, _, _): return "start:\(shortId(callId))"
        case .join(let callId, _): return "join:\(shortId(callId))"
        case .leave(let callId, _): return "leave:\(shortId(callId))"
        case .end(let callId): return "end:\(shortId(callId))"
        }
    }

    /// The first four bytes of a call id, hex — what the SFU logs too.
    static func shortId(_ callId: Data) -> String {
        callId.prefix(4).map { String(format: "%02x", $0) }.joined()
    }
}
