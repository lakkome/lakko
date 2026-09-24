// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Whether an incoming call rings, and for how long.
//!
//! `docs/ux/calls.md` section 2.1. Two of that section's rules are here
//! because both were being answered independently by each client, and one of
//! them was not being answered at all.
//!
//! ## The window
//!
//! A call rings for 45 seconds and then both sides give up. That number was
//! written out FOUR times - once in each client's call host, plus a fifth for
//! the ring surface and a sixth for the iOS placeholder - with nothing
//! holding them equal. The consequence of a drift there is not cosmetic: the
//! window is the only thing that makes "one side does not ring on after the
//! other gave up" true, so two different numbers means one side records a
//! missed call while the other is still ringing.
//!
//! ## Mute
//!
//! "A muted chat never rings" was implemented by no client at all. Mute is
//! stored and drawn everywhere - it is in the pairing bundle and it is on the
//! conversation row - and not one ring path read it. The single mention of
//! mute on any ring path was a comment in the iOS notification extension
//! asserting that a muted chat is not woken, which no code supported.
//!
//! What mute does is deliberately NOT "drop the call", and not "hide it"
//! either. A person who muted a conversation asked not to be dragged out of
//! what they are doing by it; they did not ask to stop being reachable by the
//! person. So what is withheld is exactly the interruption - the ringtone,
//! the vibration, the screen that takes over - while the call is still
//! OFFERED, on the quiet surfaces section 2.1.1 names, and is still
//! answerable for the whole window.
//!
//! The first pass got that wrong in the other direction: it read "never
//! rings" as "show nothing", which made a muted contact unanswerable and
//! turned muting a conversation into half-blocking the person.
//!
//! Pure, clock-free and platform-free: the caller supplies the age, which it
//! reads from the delivery service's own timestamp on the offer row rather
//! than from a local clock, so two devices with different clocks still agree.

/// How long a call rings before both sides give up, in milliseconds.
///
/// Shared rather than restated because it is a number two DIFFERENT devices
/// have to agree on. Section 2.1 states it as one rule for both sides, and a
/// client that carried its own would break the rule quietly: the caller would
/// still be ringing while the callee had already filed a missed call, or the
/// reverse.
pub const CALL_RING_WINDOW_MS: u64 = 45_000;

// The window has to outlast a person walking to their phone and has to be
// gone before it becomes a nuisance. Both bounds are asserted rather than
// described, so a change outside them fails the build instead of a test.
const _: () = assert!(CALL_RING_WINDOW_MS >= 20_000);
const _: () = assert!(CALL_RING_WINDOW_MS <= 90_000);

/// What an arriving call offer does to this device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RingDecision {
    /// Ring: ringtone, vibration, and the call screen that takes over.
    Ring,
    /// Offer the call without interrupting. The conversation is muted.
    ///
    /// No sound, no vibration, and no surface that takes the screen over -
    /// but the call IS offered, quietly, and stays answerable for the whole
    /// window (`docs/ux/calls.md` section 2.1.1 names the two surfaces). It
    /// is neither a dropped call nor a hidden one: muting asks not to be
    /// interrupted by a conversation, not to stop being reachable by the
    /// person in it.
    Silent,
    /// The caller has already given up. Record a missed call and show
    /// nothing: ringing here would offer an answer that connects to nobody.
    TooLate,
}

/// Whether an arriving offer rings, given the conversation's mute flag and
/// how old the offer is.
///
/// `offer_age_ms` is measured from the delivery service's own timestamp on
/// the row, never from a local clock: the two devices in a call have no
/// shared clock, and the whole point of the window is that both of them read
/// it the same way.
///
/// Age is judged BEFORE mute, and the order is load-bearing rather than
/// arbitrary. Both answers withhold the ring, but they differ in what the
/// caller does next, and a stale offer is the stronger statement of the two:
/// it says the call is over. Reporting a ten-minute-old offer as `Silent`
/// would invite a client to surface it as something still answerable.
#[must_use]
pub const fn ring_decision(muted: bool, offer_age_ms: u64) -> RingDecision {
    if offer_age_ms >= CALL_RING_WINDOW_MS {
        return RingDecision::TooLate;
    }
    if muted {
        return RingDecision::Silent;
    }
    RingDecision::Ring
}

/// How much of the ring window is left, in milliseconds.
///
/// For the platform affordances that need a deadline rather than a verdict -
/// a notification that takes itself down, a ring screen that closes itself.
/// Saturates at zero, so a clock that jumped backwards on the server cannot
/// produce a ring that lasts longer than the window.
#[must_use]
pub const fn remaining_ring_ms(offer_age_ms: u64) -> u64 {
    CALL_RING_WINDOW_MS.saturating_sub(offer_age_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_offer_on_an_unmuted_conversation_rings() {
        assert_eq!(ring_decision(false, 0), RingDecision::Ring);
        assert_eq!(ring_decision(false, 44_999), RingDecision::Ring);
    }

    /// The rule that was implemented nowhere - and note what `Silent` means:
    /// offered quietly and still answerable, never hidden.
    #[test]
    fn a_muted_conversation_never_rings() {
        for age in [0, 1, 1_000, 44_999] {
            assert_eq!(ring_decision(true, age), RingDecision::Silent, "age={age}");
        }
    }

    /// And it is not the same as dropping the call: `Silent` is a distinct
    /// answer from `TooLate`, so a client cannot collapse "do not interrupt"
    /// into "this never happened".
    #[test]
    fn muted_is_not_the_same_answer_as_over() {
        assert_ne!(
            ring_decision(true, 0),
            ring_decision(false, CALL_RING_WINDOW_MS)
        );
    }

    #[test]
    fn an_offer_past_the_window_never_rings_muted_or_not() {
        for muted in [false, true] {
            assert_eq!(
                ring_decision(muted, CALL_RING_WINDOW_MS),
                RingDecision::TooLate,
                "muted={muted}"
            );
            assert_eq!(
                ring_decision(muted, u64::MAX),
                RingDecision::TooLate,
                "muted={muted}"
            );
        }
    }

    /// The boundary is exclusive at the window: at exactly 45 s the caller
    /// has given up, so this side must not still be offering an answer.
    #[test]
    fn the_window_boundary_belongs_to_the_caller_giving_up() {
        assert_eq!(
            ring_decision(false, CALL_RING_WINDOW_MS - 1),
            RingDecision::Ring
        );
        assert_eq!(
            ring_decision(false, CALL_RING_WINDOW_MS),
            RingDecision::TooLate
        );
    }

    #[test]
    fn the_remaining_time_runs_out_and_never_goes_negative() {
        assert_eq!(remaining_ring_ms(0), CALL_RING_WINDOW_MS);
        assert_eq!(remaining_ring_ms(CALL_RING_WINDOW_MS), 0);
        assert_eq!(remaining_ring_ms(u64::MAX), 0);
    }

    /// A ring that is still allowed always has time left on it, and one that
    /// is not has none - so a client can drive a deadline from this without
    /// having to re-derive the verdict.
    #[test]
    fn the_verdict_and_the_deadline_never_disagree() {
        for age in [0, 1, 22_500, 44_999, 45_000, 45_001, u64::MAX] {
            let rings = ring_decision(false, age) == RingDecision::Ring;
            assert_eq!(rings, remaining_ring_ms(age) > 0, "age={age}");
        }
    }
}
