// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The pure half of the system incoming-call experience (`docs/ux/calls.md`
/// sections 2.1 and 2.2): what a wake payload says, what the system call screen
/// is told, and which of the two incoming-call surfaces gets to show.
///
/// Everything here is a value or a total function over values — no CallKit, no
/// PushKit, no WebRTC, no `UIApplication` — so every rule below is exercised by
/// `LakkoTests/CallKitIncomingTests.swift` without a device, an APNs
/// certificate or a running call. The classes that own the platform objects
/// (``CallKitProvider``, ``VoipPushRegistrar``, ``CallKitBridge``) hold no
/// decisions of their own; they read them from here.

// MARK: - What the callee is told the call is

/// The one line the callee's screen says about the kind of call
/// (`docs/ux/calls.md` section 2.2: "one line saying what it is: `Voice call`
/// or `Video call`").
///
/// The notification-service extension carries its OWN copy of these two
/// strings (``nseCallKindLabel(video:)``) because it is a separate process that
/// links neither this module nor the full FFI. That duplication is deliberate
/// and is the same posture the NSE already takes for the call-signaling byte
/// peek: two processes, one wording, each stated where it is used.
public enum CallKindLabel {
    /// `Voice call` / `Video call` — never "audio", never a protocol word.
    public static func text(video: Bool) -> String {
        video ? "Video call" : "Voice call"
    }
}

// MARK: - The fields the system call screen shows

/// What the system call screen is told about a call: who is calling and
/// whether they asked for video. Mapped onto a `CXCallUpdate` by
/// ``CallKitProvider``; kept as a plain value so the mapping is testable.
public struct CallKitCallUpdate: Equatable {
    /// The name on the lock screen. Never empty — a call with no resolved name
    /// still has to say something.
    public let localizedCallerName: String
    /// `true` when the offer asked for video, so the system screen offers the
    /// video answer affordance and the call is filed as a video call.
    public let hasVideo: Bool

    public init(localizedCallerName: String, hasVideo: Bool) {
        self.localizedCallerName = localizedCallerName
        self.hasVideo = hasVideo
    }
}

/// How a call becomes a ``CallKitCallUpdate``, and how a 16-byte Lakko call id
/// becomes the `UUID` CallKit identifies a call by.
public enum IncomingCallReport {

    /// What the system screen says before anything has been decrypted.
    ///
    /// PushKit requires the call to be reported to CallKit **before** the push
    /// handler returns, and at that instant the payload carries nothing but an
    /// opaque group pseudonym and a journal pointer (invariants #2 and #5) —
    /// there is no caller and no call kind in it, by design. So the first
    /// report is deliberately vague rather than invented, and the real name and
    /// kind arrive moments later as a call update. Saying "Incoming call" and
    /// then correcting it is honest; guessing a name would not be.
    public static let unknownCaller = "Incoming call"

    /// The placeholder report: an unnamed voice call.
    ///
    /// `hasVideo` is **false** in the placeholder because the system screen's
    /// video affordance cannot be taken away by a later update on every iOS
    /// version — offering video for a voice call is a promise the call cannot
    /// keep, while adding it a moment later is free.
    public static func placeholder() -> CallKitCallUpdate {
        CallKitCallUpdate(localizedCallerName: unknownCaller, hasVideo: false)
    }

    /// The report once the offer has been decrypted: the resolved name if there
    /// is one, else the same placeholder wording. A blank or whitespace-only
    /// name is treated as no name at all.
    public static func resolved(callerName: String?, video: Bool) -> CallKitCallUpdate {
        let trimmed = callerName?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return CallKitCallUpdate(
            localizedCallerName: trimmed.isEmpty ? unknownCaller : trimmed,
            hasVideo: video
        )
    }

    /// The `UUID` CallKit knows a call by, derived from the call's own 16-byte
    /// id rather than minted.
    ///
    /// A Lakko call id is exactly 16 random bytes and a `UUID` is exactly 16
    /// bytes, so the mapping is the identity — which is what makes it
    /// **derivable from both ends**: the VoIP push path reports a call under
    /// this UUID and the signaling path, arriving later through a completely
    /// different route, computes the same UUID and recognises the call as
    /// already reported instead of ringing a second time. A minted UUID would
    /// need a lookup table shared across a process launch.
    public static func uuid(forCallId callId: Data) -> UUID? {
        guard callId.count == 16 else { return nil }
        let b = [UInt8](callId)
        return UUID(uuid: (
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
            b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
        ))
    }

    /// The inverse of ``uuid(forCallId:)`` — the 16 call-id bytes behind a
    /// CallKit UUID, so an action arriving from the system UI names a call the
    /// app can find.
    public static func callId(forUuid uuid: UUID) -> Data {
        withUnsafeBytes(of: uuid.uuid) { Data($0) }
    }

    /// Lowercase hex of a call id — the key both the notification payload and
    /// the app's own bookkeeping use to name one call.
    public static func hex(_ bytes: Data) -> String {
        let alphabet = Array("0123456789abcdef")
        var out = String()
        out.reserveCapacity(bytes.count * 2)
        for byte in bytes {
            out.append(alphabet[Int(byte >> 4)])
            out.append(alphabet[Int(byte & 0x0F)])
        }
        return out
    }
}

// MARK: - The wake payload

/// A push wake as it arrives from `services/push-gw`, decoded.
///
/// The payload carries only what invariants #2 and #5 allow: an opaque group
/// routing id (`g`), a DS journal pointer (`p`) and, since the call-wake work,
/// a one-character kind tag (`k`, `"c"` for a call). There is no caller, no
/// call id and no media kind in it — those come from decrypting the journal row
/// the pointer names.
public struct VoipWakePayload: Equatable {
    /// 64 lowercase-hex chars of the opaque `push_gid`, or `nil`.
    public let groupHex: String?
    /// The DS message id that triggered the wake, or `nil`.
    public let pointer: Int64?
    /// `true` when the gateway marked this a call wake (`k == "c"`).
    public let isCallWake: Bool

    public init(groupHex: String?, pointer: Int64?, isCallWake: Bool) {
        self.groupHex = groupHex
        self.pointer = pointer
        self.isCallWake = isCallWake
    }

    /// Decode a `PKPushPayload` / `UNNotificationContent` `userInfo` dictionary.
    /// Every field is optional and a malformed one is simply absent — a wake is
    /// never rejected for a field the handler does not strictly need, because a
    /// VoIP push that is not reported to CallKit terminates the app.
    public static func parse(_ userInfo: [AnyHashable: Any]) -> VoipWakePayload {
        let groupHex = (userInfo["g"] as? String).flatMap { $0.isEmpty ? nil : $0 }
        let pointer: Int64?
        if let number = userInfo["p"] as? NSNumber {
            pointer = number.int64Value
        } else if let string = userInfo["p"] as? String, let value = Int64(string) {
            pointer = value
        } else {
            pointer = nil
        }
        return VoipWakePayload(
            groupHex: groupHex,
            pointer: pointer,
            isCallWake: (userInfo["k"] as? String) == "c"
        )
    }
}

// MARK: - Which incoming-call surface shows

/// Where the app learned about an incoming call.
public enum IncomingCallSource: Equatable {
    /// A PushKit VoIP push. iOS requires a call to be reported to CallKit
    /// before the handler returns, on pain of killing the app.
    case voipPush
    /// A call-signaling row the app decrypted itself, on its own receive path.
    case signalingRow
}

/// Which incoming-call surface to show (`docs/ux/calls.md` section 2.1).
public enum IncomingCallUi: Equatable {
    /// The phone's own call screen: ringtone, lock-screen answer, call history.
    case systemCallUi
    /// Lakko's own incoming-call surface over the running app (section 2.2).
    case inApp
    /// Nothing new — this call is already ringing somewhere.
    case none
    /// Neither of the two screens that take over: the conversation is muted
    /// (section 2.1.1).
    ///
    /// Not the same as ``none``. The call is ringing and answerable, and is
    /// offered on the two quiet, in-layout surfaces
    /// (``QuietIncomingCallPresentation``) — what this withholds is the
    /// interruption, so nothing here is reported to the phone and nothing is
    /// drawn over what the person is reading.
    case quiet
}

/// The one rule for choosing between the incoming-call surfaces.
///
/// Four facts decide it, and none of them is "which code path noticed first":
///
///   * **A VoIP push is not a choice.** iOS terminates an app that takes a VoIP
///     push without reporting a call, so a VoIP push always reports, foreground
///     or not. Anything else is a crash waiting for a background launch.
///   * **A call already reported must not ring twice.** The signaling row for a
///     call the VoIP push already reported arrives moments later on the app's
///     own receive path; without this the user would get the system call screen
///     AND the in-app surface for one call.
///   * **An active app shows its own screen.** Section 2.1's first row: with the
///     app open, the call takes over Lakko's own screen, not the phone's.
///   * **A muted conversation takes over neither.** Section 2.1.1: the call is
///     offered in the chat and in the conversation list, both in the layout, so
///     nothing the person was reading moves or is covered.
public enum IncomingCallPresentation {

    /// Whether Lakko's own full-screen incoming call should be on screen
    /// (`docs/ux/calls.md` section 2.2).
    ///
    /// The second argument is what keeps this off the CallKit path. ``decide``
    /// already chooses ONE surface per call, but it runs once, when the ring
    /// starts; the view re-evaluates continuously and would happily draw the
    /// in-app screen underneath the system call screen — two surfaces for one
    /// call, which is exactly what section 2.1 forbids. So the choice
    /// ``decide`` made is recorded, and this reads it back.
    ///
    /// - Parameters:
    ///   - state: the 1x1 call's state; only a ringing call has an incoming
    ///     screen at all.
    ///   - ringingOnSystemUi: this call was reported to CallKit, so the phone's
    ///     own screen owns it.
    ///   - quiet: the conversation is muted (section 2.1.1), so no surface may
    ///     take the screen over — the call is offered in the chat and in the
    ///     list instead. Defaulted `false` for the same reason the routing
    ///     rule's `muted` is: a caller that cannot answer the question should
    ///     ring rather than go quiet, which is section 2.1's own fallback.
    public static func showsInAppScreen(
        state: CallSession.WebCallState?, ringingOnSystemUi: Bool, quiet: Bool = false
    ) -> Bool {
        state == .ringing && !ringingOnSystemUi && !quiet
    }

    /// - Parameter quiet: the conversation is muted (section 2.1.1). Defaulted
    ///   `false` — absent means not muted, which rings, the direction a missing
    ///   answer has to fail in.
    public static func decide(
        source: IncomingCallSource,
        appIsActive: Bool,
        alreadyReported: Bool,
        quiet: Bool = false
    ) -> IncomingCallUi {
        switch source {
        case .voipPush:
            // Still not a choice, and mute does not change it: iOS terminates
            // an app that takes a VoIP push without reporting a call. The
            // muted case is handled by reporting as demanded and then ending
            // the call at once (``VoipPushRegistrar``), never by skipping the
            // report.
            return .systemCallUi
        case .signalingRow:
            // `alreadyReported` is read BEFORE `quiet`, and the order is the
            // decision. It means the phone is ALREADY ringing this call from a
            // VoIP push, on a path that read the same mute flag and chose to
            // ring — which happens when that read could not resolve the
            // conversation, and section 2.1's fallback for an unreadable mute
            // flag is to ring. Answering `.quiet` here would leave a call
            // CallKit is showing with nothing left to update it, so the
            // already-ringing call keeps its surface.
            if alreadyReported { return .none }
            if quiet { return .quiet }
            return appIsActive ? .inApp : .systemCallUi
        }
    }
}
