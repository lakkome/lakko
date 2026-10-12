// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0065 R.12 / UX 16.4 - a live ONE-TO-ONE call ends when its conversation
// moves, and is never started in a dead session.
//
// iOS mirror of `apps/web/src/calls/call-continuity.ts`; the tables are the
// same and so are the cases in `CallContinuityTests`.
//
// A one-to-one call takes its keys and its signalling from the one-to-one's
// group. Once the other person's reset is accepted that group's other leaves
// are removed devices: the media is going to a device that is no longer the
// person's. The call ends the way a call ends when the other side drops, with
// no copy of its own (R.0); the next call uses the conversation's successor.
//
// A group call is not ended here: its members send where their devices are
// (R.7 case 3), and a group call's roster is the SFU's.
public enum CallContinuity {

    /// | a one-to-one call is live in group G | G is unrepairable for an accepted reset | the conversation's head is G | ends |
    /// |--------------------------------------|------------------------------------------|------------------------------|------|
    /// | no                                   | -                                        | -                            | no   |
    /// | yes                                  | yes                                      | any                          | yes  |
    /// | yes                                  | no                                       | no (it moved)                | yes  |
    /// | yes                                  | no                                       | yes, or unknown              | no   |
    ///
    /// - Parameters:
    ///   - callGroupIdB64: the group the live one-to-one call runs in; `nil`
    ///     when none is live.
    ///   - unrepairableKeyB64: the "unrepairable by" key over that group.
    ///   - headGroupIdB64: the head group of that group's conversation, or
    ///     `nil` when unknown.
    public static func oneToOneCallEndsOnMove(
        callGroupIdB64: String?, unrepairableKeyB64: String?, headGroupIdB64: String?
    ) -> Bool {
        guard let callGroupIdB64 else { return false }
        if unrepairableKeyB64 != nil { return true }
        guard let headGroupIdB64 else { return false }
        return headGroupIdB64 != callGroupIdB64
    }

    /// Where a one-to-one call the person starts is placed.
    public enum Target: Equatable {
        /// Dial in this group.
        case dial(groupIdB64: String)
        /// No session the other person's devices are in exists on this device
        /// yet: the call does not start, exactly as a call that cannot start
        /// anywhere else (no copy of its own, R.0).
        case unavailable
    }

    /// ADR-0065 R.12 - a one-to-one call is never STARTED in a dead session.
    ///
    /// A call takes its keys and its signalling from the group it is dialled
    /// in. In a one-to-one whose other person's reset was accepted, that
    /// group's other leaves are removed devices: an offer there rings nobody
    /// the person is. So the call is placed in the conversation's successor
    /// when this device holds one, and otherwise it does not start.
    ///
    /// | the asked group is unrepairable | the conversation's head | the head is unrepairable | target        |
    /// |---------------------------------|-------------------------|--------------------------|---------------|
    /// | no                              | the asked group/unknown | -                        | dial asked    |
    /// | no                              | another group           | no                       | dial the head |
    /// | no                              | another group           | yes                      | unavailable   |
    /// | yes                             | the asked group/unknown | -                        | unavailable   |
    /// | yes                             | another group           | no                       | dial the head |
    /// | yes                             | another group           | yes                      | unavailable   |
    ///
    /// Rows 2 and 3: a view still open on an older segment after the
    /// conversation moved dials where the conversation is now.
    ///
    /// - Parameters:
    ///   - groupIdB64: the group the call was asked for.
    ///   - unrepairableKeyB64: the "unrepairable by" key over that group.
    ///   - headGroupIdB64: the head group of that group's conversation, or
    ///     `nil` when unknown.
    ///   - headUnrepairableKeyB64: the "unrepairable by" key over the head
    ///     (`nil` when the head is the asked group, is unknown, or is
    ///     repairable).
    public static func oneToOneCallTarget(
        groupIdB64: String,
        unrepairableKeyB64: String?,
        headGroupIdB64: String?,
        headUnrepairableKeyB64: String?
    ) -> Target {
        if let headGroupIdB64, headGroupIdB64 != groupIdB64 {
            return headUnrepairableKeyB64 == nil ? .dial(groupIdB64: headGroupIdB64) : .unavailable
        }
        return unrepairableKeyB64 == nil ? .dial(groupIdB64: groupIdB64) : .unavailable
    }

    /// Thrown by the call host's dial for `Target.unavailable`: nothing was
    /// signalled anywhere. Callers treat it as any other call that could not
    /// start.
    public struct UnavailableInSession: Error, Equatable {
        public init() {}
    }
}
