// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The collapsed call bar: whether it is on screen, and what its one line
//! says.
//!
//! `docs/ux/calls.md` section 4.5 puts a live call away into a bar across the
//! top of the app so the app stays usable while the call runs. Two decisions
//! follow it everywhere and neither has a platform in it, so they are written
//! here rather than three times: whether the bar is showing, and which of the
//! four things its status line says.
//!
//! The rule the clients keep getting wrong on their own is the second one.
//! Section 4 says the status line carries the state or the running time and
//! **never two at once**; expressed as three independent booleans in a view,
//! that invariant is one careless `if` away from breaking. Here it is a sum
//! type, so it cannot say two things.

/// Where a call is, as the bar needs to see it.
///
/// This is the union of the three clients' own phases, with the two that do
/// not reach the bar included deliberately rather than left out: a caller
/// must be able to hand this rule whatever phase it holds and be told the
/// answer, instead of deciding for itself which phases are worth asking
/// about - deciding that is the mistake this rule exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallBarPhase {
    /// No call at all.
    Idle,
    /// An incoming call being offered, which the person has not answered.
    Ringing,
    /// Built, not yet on the wire. `docs/ux/calls.md` section 4 shows this as
    /// `Calling...`: an outgoing call is dialled in the same breath as it is
    /// built, so a separate word for it would name an instant nobody sees.
    Preparing,
    /// Ringing out: the offer is on the wire, nobody has answered.
    Calling,
    /// Answered; the transport is coming up.
    Connecting,
    /// Media is flowing.
    Connected,
    /// The transport dropped and is being rebuilt.
    Reconnecting,
    /// Over.
    Ended,
}

/// Whether the collapsed bar is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallBar {
    /// Nothing to show.
    Hidden,
    /// The bar is up, above whatever screen is showing.
    Shown,
}

/// What the bar's one status line says.
///
/// Four variants and no way to hold two, which is the point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallBarStatus {
    /// `Calling...`
    Calling,
    /// `Connecting...`
    Connecting,
    /// `Reconnecting...`
    Reconnecting,
    /// The running time, in milliseconds since the call connected. The client
    /// formats it; how long it has been is not a presentation decision.
    Elapsed(u64),
}

/// Whether the collapsed bar is on screen, given where the call is and
/// whether the call screen is up.
///
/// Three reasons it is hidden, and each is a sentence of section 4.5 rather
/// than an optimisation:
///
/// - the call screen is up. One state, one place: the same rule that keeps a
///   floating pill off the call screen in section 4.
/// - the call is only ringing in. An incoming call is answered or declined,
///   and a muted conversation's call is already offered without covering
///   anything (section 2.1.1), so there is nothing to put away.
/// - there is no call, or it is over. When a collapsed call ends the bar goes
///   away and the conversation gets its usual row; the `Call ended` overlay
///   belongs to the call screen, and a person who put the call away asked not
///   to be interrupted by it.
///
/// An OUTGOING call collapses from the moment it is placed, which is why
/// `Preparing` and `Calling` are shown rather than waiting for `Connected`.
#[must_use]
pub const fn call_bar_visibility(phase: CallBarPhase, call_screen_up: bool) -> CallBar {
    if call_screen_up {
        return CallBar::Hidden;
    }
    match phase {
        CallBarPhase::Idle | CallBarPhase::Ringing | CallBarPhase::Ended => CallBar::Hidden,
        CallBarPhase::Preparing
        | CallBarPhase::Calling
        | CallBarPhase::Connecting
        | CallBarPhase::Connected
        | CallBarPhase::Reconnecting => CallBar::Shown,
    }
}

/// What the bar's status line says.
///
/// `connected_for_ms` is how long media has been flowing; it is read only
/// when the call is connected, so a caller that has no clock yet may pass
/// anything.
///
/// `Preparing` answers `Calling` for the reason given on the variant: the
/// instant between building a call and putting it on the wire is not a state
/// a person is shown.
#[must_use]
pub const fn call_bar_status(phase: CallBarPhase, connected_for_ms: u64) -> CallBarStatus {
    match phase {
        CallBarPhase::Connected => CallBarStatus::Elapsed(connected_for_ms),
        CallBarPhase::Connecting => CallBarStatus::Connecting,
        CallBarPhase::Reconnecting => CallBarStatus::Reconnecting,
        // Idle, Ringing and Ended never reach a shown bar; answering
        // `Calling` for them is unreachable rather than meaningful, and is
        // the only arm that cannot mislead if one ever does.
        CallBarPhase::Idle
        | CallBarPhase::Ringing
        | CallBarPhase::Preparing
        | CallBarPhase::Calling
        | CallBarPhase::Ended => CallBarStatus::Calling,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERY_PHASE: [CallBarPhase; 8] = [
        CallBarPhase::Idle,
        CallBarPhase::Ringing,
        CallBarPhase::Preparing,
        CallBarPhase::Calling,
        CallBarPhase::Connecting,
        CallBarPhase::Connected,
        CallBarPhase::Reconnecting,
        CallBarPhase::Ended,
    ];

    /// One state, one place. The bar exists because the call screen is away;
    /// showing both would be the floating pill section 4 refuses.
    #[test]
    fn the_bar_is_never_up_while_the_call_screen_is() {
        for phase in EVERY_PHASE {
            assert_eq!(
                call_bar_visibility(phase, true),
                CallBar::Hidden,
                "{phase:?} showed a bar over the call screen"
            );
        }
    }

    /// A call that is only being offered is answered or declined. There is
    /// nothing to put away, and a bar for it would be a third surface beside
    /// the two section 2.1.1 already gives a muted conversation.
    #[test]
    fn an_incoming_call_being_rung_has_no_bar() {
        assert_eq!(
            call_bar_visibility(CallBarPhase::Ringing, false),
            CallBar::Hidden
        );
    }

    /// An outgoing call collapses from the moment it is placed - the bar does
    /// not wait for the other side to answer.
    #[test]
    fn an_outgoing_call_has_a_bar_before_anyone_answers() {
        assert_eq!(
            call_bar_visibility(CallBarPhase::Preparing, false),
            CallBar::Shown
        );
        assert_eq!(
            call_bar_visibility(CallBarPhase::Calling, false),
            CallBar::Shown
        );
    }

    #[test]
    fn a_live_call_has_a_bar_and_a_finished_one_does_not() {
        assert_eq!(
            call_bar_visibility(CallBarPhase::Connected, false),
            CallBar::Shown
        );
        assert_eq!(
            call_bar_visibility(CallBarPhase::Reconnecting, false),
            CallBar::Shown
        );
        assert_eq!(
            call_bar_visibility(CallBarPhase::Ended, false),
            CallBar::Hidden
        );
        assert_eq!(
            call_bar_visibility(CallBarPhase::Idle, false),
            CallBar::Hidden
        );
    }

    /// The running time belongs to a connected call and to nothing else. A
    /// caller with no clock may pass anything for the other phases, so the
    /// rule must not read it.
    #[test]
    fn only_a_connected_call_reports_a_running_time() {
        for phase in EVERY_PHASE {
            let status = call_bar_status(phase, 12_345);
            if phase == CallBarPhase::Connected {
                assert_eq!(status, CallBarStatus::Elapsed(12_345));
            } else {
                assert_ne!(
                    status,
                    CallBarStatus::Elapsed(12_345),
                    "{phase:?} reported a running time"
                );
            }
        }
    }

    /// The elapsed value is carried through rather than interpreted: how long
    /// a call has been running is not a presentation decision, and a rule
    /// that rounded or clamped it here would be making one.
    #[test]
    fn the_running_time_is_carried_through_untouched() {
        for ms in [0, 1, 999, 1_000, 86_400_000, u64::MAX] {
            assert_eq!(
                call_bar_status(CallBarPhase::Connected, ms),
                CallBarStatus::Elapsed(ms)
            );
        }
    }

    /// The invariant the sum type exists to hold: section 4 says the line
    /// carries the state or the time and never two at once. Expressed as
    /// three booleans in a view this is one careless `if` from breaking; here
    /// it cannot be expressed at all.
    #[test]
    fn the_status_line_says_exactly_one_thing() {
        for phase in EVERY_PHASE {
            let status = call_bar_status(phase, 7);
            let word_count = u8::from(matches!(
                status,
                CallBarStatus::Calling | CallBarStatus::Connecting | CallBarStatus::Reconnecting
            ));
            let time_count = u8::from(matches!(status, CallBarStatus::Elapsed(_)));
            assert_eq!(word_count + time_count, 1, "{phase:?}");
        }
    }
}
