// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The shared rules of [ADR-0091] phase 2 (M4): when a device is behind in a
//! conversation, when it asks to come back, by which route, and when a
//! re-admitter accepts its request.
//!
//! Pure functions of what the client observed. They are written once here
//! and reached over both FFI surfaces so no client hand-writes them; the
//! three clients had drifted apart on every similar rule they wrote three
//! times. Nothing here reads a clock or a store: the caller passes `now_ms`
//! and its own facts.
//!
//! ## Behind: suspected and confirmed (section 1)
//!
//! ```text
//! suspected := not own_commit_owed
//!          and ( (drained and latest_epoch > local_epoch)
//!                or refused_reads >= 3 )
//! confirmed := suspected and evidence
//! evidence  := a failed row framed above local_epoch since the last
//!              applied row
//!           or a publish refused 401 with the clock not skewed
//!           or suspected has held over reads spanning 10 minutes
//!           or the suspicion came from refused_reads
//! cleared   := drained and latest_epoch <= local_epoch
//! ```
//!
//! An observation that neither suspects nor clears (an undrained read, a
//! read with no `latest_epoch`, an own commit owed) leaves the state as it
//! was. Re-entry landing clears it too; that is the caller's reset to
//! [`BehindState::InStep`].
//!
//! Every number here is chosen, not measured (ADR-0091 open question 5).
//!
//! [ADR-0091]: ../../../docs/adr/0091-conversation-rejoin.md

use crate::group_kind::GroupKind;

/// Consecutive refused reads (`401`, clock not skewed, signed at the
/// device's current key) that make a device suspect it is behind. Chosen,
/// not measured.
pub const REFUSED_READS_TO_SUSPECT: u32 = 3;

/// How long a suspicion must hold, over reads, before it is shown without
/// other evidence. Chosen, not measured.
pub const SUSPICION_CONFIRM_AFTER_MS: u64 = 10 * 60 * 1000;

/// How often a confirmed device repeats its request to come back (ADR-0081's
/// re-knock interval). Chosen, not measured.
pub const REENTRY_REQUEST_INTERVAL_MS: u64 = 30 * 60 * 1000;

/// A re-admitter refuses a re-entry for a leaf it decrypted an application
/// message from, sealed at the current epoch, within this window (that leaf
/// is not behind). Chosen, not measured.
pub const REENTRY_RECENT_WRITE_WINDOW_MS: u64 = 60 * 60 * 1000;

/// A re-admitter accepts at most one re-entry per leaf within this window.
/// Chosen, not measured.
pub const REENTRY_LEAF_COOLDOWN_MS: u64 = 60 * 60 * 1000;

/// How many re-entry requests per conversation a device keeps the
/// KeyPackage private keys of (the last two).
pub const REENTRY_REQUESTS_KEPT: usize = 2;

/// Where a device stands in one conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BehindState {
    /// Nothing suggests the device is behind.
    InStep,
    /// The device stops acting in the conversation and asks to come back.
    Suspected {
        /// When the suspicion began.
        since_ms: u64,
    },
    /// Shown to the person: the composer is replaced by the waiting note.
    Confirmed {
        /// When the suspicion began.
        since_ms: u64,
    },
}

impl BehindState {
    /// `true` when the device must stop committing, sweeping, registering,
    /// proposing and sending repeat envelopes in the conversation (section
    /// 2, C-93).
    #[must_use]
    pub const fn stops_acting(self) -> bool {
        !matches!(self, Self::InStep)
    }

    /// `true` when the person is shown the waiting note and nothing is sent.
    #[must_use]
    pub const fn is_confirmed(self) -> bool {
        matches!(self, Self::Confirmed { .. })
    }
}

/// What one read of the conversation journal (and its bookkeeping) showed.
///
/// A record of independent yes/no facts the client observed, one per clause
/// of section 1, so the bools are the honest shape.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BehindObservation {
    /// An ADR-0040 outbox entry or a staged sweep awaits a verdict.
    pub own_commit_owed: bool,
    /// The read returned no row and no "more", and the cursor was not held.
    pub drained: bool,
    /// The DS's `latest_epoch` from this read, if it named one.
    pub latest_epoch: Option<u64>,
    /// The group's own epoch.
    pub local_epoch: u64,
    /// Consecutive `401` on reads signed at the current key, clock not
    /// skewed.
    pub refused_reads: u32,
    /// Since the last applied row, a row framed at an epoch above
    /// `local_epoch` failed to process.
    pub failed_row_above_local: bool,
    /// A publish of this device to this journal was refused `401` with the
    /// clock not skewed.
    pub publish_refused: bool,
}

/// The next [`BehindState`] after one observation (section 1).
#[must_use]
pub const fn conversation_behind_verdict(
    previous: BehindState,
    observation: BehindObservation,
    now_ms: u64,
) -> BehindState {
    let o = observation;
    let behind_by_read = match o.latest_epoch {
        Some(latest) => o.drained && latest > o.local_epoch,
        None => false,
    };
    let cleared = match o.latest_epoch {
        Some(latest) => o.drained && latest <= o.local_epoch,
        None => false,
    };
    if cleared {
        return BehindState::InStep;
    }
    let by_refused_reads = o.refused_reads >= REFUSED_READS_TO_SUSPECT;
    let suspected_now = !o.own_commit_owed && (behind_by_read || by_refused_reads);
    if !suspected_now {
        return previous;
    }
    let since_ms = match previous {
        BehindState::InStep => now_ms,
        BehindState::Suspected { since_ms } | BehindState::Confirmed { since_ms } => since_ms,
    };
    let held_long_enough = now_ms.saturating_sub(since_ms) >= SUSPICION_CONFIRM_AFTER_MS;
    let evidence =
        o.failed_row_above_local || o.publish_refused || held_long_enough || by_refused_reads;
    if evidence || matches!(previous, BehindState::Confirmed { .. }) {
        BehindState::Confirmed { since_ms }
    } else {
        BehindState::Suspected { since_ms }
    }
}

/// `true` when the device should send (or repeat) its request to come back.
///
/// A request goes out as soon as the device is suspected (section 1: a
/// suspected device stops acting and asks to be brought back), and is
/// repeated no more often than every [`REENTRY_REQUEST_INTERVAL_MS`] while it
/// stays behind. Never while in step.
#[must_use]
pub const fn reentry_request_due(
    state: BehindState,
    last_sent_ms: Option<u64>,
    now_ms: u64,
) -> bool {
    if !state.stops_acting() {
        return false;
    }
    match last_sent_ms {
        None => true,
        Some(last) => now_ms.saturating_sub(last) >= REENTRY_REQUEST_INTERVAL_MS,
    }
}

/// The route a request to come back takes (section 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReentryRoute {
    /// Route 1: a DeviceGroup service envelope to another device of the
    /// same account that is a member of the conversation.
    OwnDevice,
    /// Route 2: a sealed mailbox row to the other person of a one-to-one.
    Mailbox,
    /// Route 3: a sealed knock row to the group's knock key.
    Knock,
}

/// Which route applies, in order: another own device in the conversation,
/// else a one-to-one's mailbox, else the group's knock queue.
#[must_use]
pub const fn reentry_route(own_device_in_conversation: bool, kind: GroupKind) -> ReentryRoute {
    if own_device_in_conversation {
        return ReentryRoute::OwnDevice;
    }
    match kind {
        GroupKind::OneToOne => ReentryRoute::Mailbox,
        GroupKind::Group => ReentryRoute::Knock,
    }
}

/// What a re-admitter knows about a request beyond what the core checks
/// ([`crate::Group::reentry_request_target`] checks the KeyPackage, its
/// marker, the leaf it replaces and the account).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReentryAdmissionFacts {
    /// The device is still the account's: in the re-admitter's own
    /// DeviceGroup roster (route 1), or listed by the account's directory
    /// entry and not tombstoned (routes 2 and 3).
    pub device_still_the_accounts: bool,
    /// When this device last decrypted an application message from the leaf,
    /// sealed at the current epoch, if ever.
    pub last_current_epoch_message_from_leaf_ms: Option<u64>,
    /// This device already handled a re-entry for the leaf at this epoch.
    pub handled_for_leaf_at_this_epoch: bool,
    /// When this device last accepted a re-entry for the leaf, if ever.
    pub last_accepted_for_leaf_ms: Option<u64>,
}

/// A re-admitter's answer to a request (section 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReentryAdmission {
    /// Propose the replacement.
    Admit,
    /// The device is no longer the account's (removed, or unknown).
    NotTheAccountsDevice,
    /// The leaf wrote at the current epoch within the window: not behind.
    LeafIsWriting,
    /// A re-entry for this leaf at this epoch was already handled.
    AlreadyHandled,
    /// A re-entry for this leaf was accepted within the cooldown.
    TooSoon,
}

/// Whether a re-admitter accepts a request whose KeyPackage the core already
/// accepted (section 4).
#[must_use]
pub const fn reentry_admission_verdict(
    facts: ReentryAdmissionFacts,
    now_ms: u64,
) -> ReentryAdmission {
    if !facts.device_still_the_accounts {
        return ReentryAdmission::NotTheAccountsDevice;
    }
    if let Some(at) = facts.last_current_epoch_message_from_leaf_ms {
        if now_ms.saturating_sub(at) < REENTRY_RECENT_WRITE_WINDOW_MS {
            return ReentryAdmission::LeafIsWriting;
        }
    }
    if facts.handled_for_leaf_at_this_epoch {
        return ReentryAdmission::AlreadyHandled;
    }
    if let Some(at) = facts.last_accepted_for_leaf_ms {
        if now_ms.saturating_sub(at) < REENTRY_LEAF_COOLDOWN_MS {
            return ReentryAdmission::TooSoon;
        }
    }
    ReentryAdmission::Admit
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const MIN: u64 = 60 * 1000;

    fn behind_read(local: u64, latest: u64) -> BehindObservation {
        BehindObservation {
            drained: true,
            latest_epoch: Some(latest),
            local_epoch: local,
            ..BehindObservation::default()
        }
    }

    #[test]
    fn a_drained_read_above_the_local_epoch_suspects_and_ten_minutes_confirm() {
        let s = conversation_behind_verdict(BehindState::InStep, behind_read(6, 16), 1000);
        assert_eq!(s, BehindState::Suspected { since_ms: 1000 });
        let s = conversation_behind_verdict(s, behind_read(6, 16), 1000 + 9 * MIN);
        assert_eq!(s, BehindState::Suspected { since_ms: 1000 });
        let s = conversation_behind_verdict(s, behind_read(6, 16), 1000 + 10 * MIN);
        assert_eq!(s, BehindState::Confirmed { since_ms: 1000 });
    }

    #[test]
    fn evidence_confirms_at_once() {
        let mut o = behind_read(6, 7);
        o.failed_row_above_local = true;
        assert!(conversation_behind_verdict(BehindState::InStep, o, 5).is_confirmed());
        let mut o = behind_read(6, 7);
        o.publish_refused = true;
        assert!(conversation_behind_verdict(BehindState::InStep, o, 5).is_confirmed());
        let o = BehindObservation {
            refused_reads: 3,
            local_epoch: 6,
            ..BehindObservation::default()
        };
        assert!(conversation_behind_verdict(BehindState::InStep, o, 5).is_confirmed());
        let o = BehindObservation {
            refused_reads: 2,
            local_epoch: 6,
            ..BehindObservation::default()
        };
        assert_eq!(
            conversation_behind_verdict(BehindState::InStep, o, 5),
            BehindState::InStep
        );
    }

    #[test]
    fn behind_verdict_clears_when_the_journal_is_back_at_the_local_epoch() {
        let confirmed = BehindState::Confirmed { since_ms: 1 };
        assert_eq!(
            conversation_behind_verdict(confirmed, behind_read(7, 7), 99),
            BehindState::InStep
        );
        assert_eq!(
            conversation_behind_verdict(
                BehindState::Suspected { since_ms: 1 },
                behind_read(7, 6),
                99
            ),
            BehindState::InStep
        );
        // An undrained read clears nothing.
        let mut o = behind_read(7, 7);
        o.drained = false;
        assert_eq!(conversation_behind_verdict(confirmed, o, 99), confirmed);
    }

    #[test]
    fn requests_go_out_while_behind_and_repeat_every_thirty_minutes() {
        assert!(!reentry_request_due(BehindState::InStep, None, 0));
        let s = BehindState::Suspected { since_ms: 0 };
        assert!(reentry_request_due(s, None, 0));
        assert!(!reentry_request_due(s, Some(0), 29 * MIN));
        assert!(reentry_request_due(s, Some(0), 30 * MIN));
    }

    #[test]
    fn the_route_is_own_device_then_mailbox_then_knock() {
        assert_eq!(
            reentry_route(true, GroupKind::Group),
            ReentryRoute::OwnDevice
        );
        assert_eq!(
            reentry_route(true, GroupKind::OneToOne),
            ReentryRoute::OwnDevice
        );
        assert_eq!(
            reentry_route(false, GroupKind::OneToOne),
            ReentryRoute::Mailbox
        );
        assert_eq!(reentry_route(false, GroupKind::Group), ReentryRoute::Knock);
    }

    #[test]
    fn admission_refuses_in_order_and_admits_otherwise() {
        let ok = ReentryAdmissionFacts {
            device_still_the_accounts: true,
            ..ReentryAdmissionFacts::default()
        };
        let now = 10 * 60 * MIN;
        assert_eq!(reentry_admission_verdict(ok, now), ReentryAdmission::Admit);
        assert_eq!(
            reentry_admission_verdict(ReentryAdmissionFacts::default(), now),
            ReentryAdmission::NotTheAccountsDevice
        );
        let writing = ReentryAdmissionFacts {
            last_current_epoch_message_from_leaf_ms: Some(now - 59 * MIN),
            ..ok
        };
        assert_eq!(
            reentry_admission_verdict(writing, now),
            ReentryAdmission::LeafIsWriting
        );
        let old_write = ReentryAdmissionFacts {
            last_current_epoch_message_from_leaf_ms: Some(now - 60 * MIN),
            ..ok
        };
        assert_eq!(
            reentry_admission_verdict(old_write, now),
            ReentryAdmission::Admit
        );
        let handled = ReentryAdmissionFacts {
            handled_for_leaf_at_this_epoch: true,
            ..ok
        };
        assert_eq!(
            reentry_admission_verdict(handled, now),
            ReentryAdmission::AlreadyHandled
        );
        let soon = ReentryAdmissionFacts {
            last_accepted_for_leaf_ms: Some(now - MIN),
            ..ok
        };
        assert_eq!(
            reentry_admission_verdict(soon, now),
            ReentryAdmission::TooSoon
        );
    }

    fn any_state() -> impl Strategy<Value = BehindState> {
        prop_oneof![
            Just(BehindState::InStep),
            any::<u32>().prop_map(|s| BehindState::Suspected {
                since_ms: u64::from(s)
            }),
            any::<u32>().prop_map(|s| BehindState::Confirmed {
                since_ms: u64::from(s)
            }),
        ]
    }

    proptest! {
        /// ADR-0091 section 6 names this property: an own commit owed, or a
        /// read that is not drained (and no run of refused reads), never
        /// moves a device out of step.
        #[test]
        fn behind_verdict_never_fires_with_an_own_commit_owed_or_an_undrained_read(
            own_commit_owed in any::<bool>(),
            drained in any::<bool>(),
            latest in proptest::option::of(any::<u64>()),
            local in any::<u64>(),
            refused in any::<u32>(),
            failed in any::<bool>(),
            publish in any::<bool>(),
            now in any::<u64>(),
        ) {
            let o = BehindObservation {
                own_commit_owed,
                drained,
                latest_epoch: latest,
                local_epoch: local,
                refused_reads: refused,
                failed_row_above_local: failed,
                publish_refused: publish,
            };
            let next = conversation_behind_verdict(BehindState::InStep, o, now);
            if own_commit_owed || (!drained && refused < REFUSED_READS_TO_SUSPECT) {
                prop_assert_eq!(next, BehindState::InStep);
            }
        }

        /// A verdict never moves `since_ms` backward or forward once set, and
        /// never goes from confirmed to suspected.
        #[test]
        fn a_suspicion_keeps_its_start_and_never_unconfirms(
            prev in any_state(),
            local in 0u64..100,
            latest in proptest::option::of(0u64..100),
            drained in any::<bool>(),
            refused in 0u32..5,
            now in any::<u64>(),
        ) {
            let o = BehindObservation {
                drained,
                latest_epoch: latest,
                local_epoch: local,
                refused_reads: refused,
                ..BehindObservation::default()
            };
            let next = conversation_behind_verdict(prev, o, now);
            match (prev, next) {
                (BehindState::Confirmed { .. }, BehindState::Suspected { .. }) => {
                    prop_assert!(false, "confirmed went back to suspected");
                },
                (BehindState::Suspected { since_ms: a } | BehindState::Confirmed { since_ms: a },
                 BehindState::Suspected { since_ms: b } | BehindState::Confirmed { since_ms: b }) => {
                    prop_assert_eq!(a, b);
                },
                _ => {},
            }
        }
    }
}
