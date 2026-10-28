// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! When a call is over, as the SFU sees it.
//!
//! ## Why the server has to know this
//!
//! A group call's record lives in the chat's MLS group, fed by the members'
//! own Start / Join / Leave / End signals; the server relays ciphertext and
//! cannot write there. That record keeps a call alive while a member's Join
//! is fresh, and a member's app that dies publishes nothing, so after a crash
//! the call it was alone in stays "in progress" on every other phone until
//! the freshness window runs out (a few minutes), and whoever taps Join lands
//! in an empty room and sits there alone (seen live on 2026-09-20).
//!
//! The one party that knows for certain that a room is empty is this server:
//! every participant holds a signaling socket here, and a socket that dies is
//! noticed by the protocol ping within [`crate::signaling::SignalingLiveness`].
//! So the rule the product wants — **a call ends
//! [`CALL_EMPTY_GRACE`] after the last person in it leaves or drops** — is
//! decided here, once, and the members learn it two ways: a join to an ended
//! call is refused with `ErrorCode::CallEnded`, and the `/v1/rooms/{room}`
//! state route answers `ended`, so the member that learns it closes the record
//! for the chat by publishing the End signal itself.
//!
//! The grace is for a bad connection: a participant whose socket blips and
//! reconnects inside it finds the call still there. Anyone who joins inside
//! it keeps the call alive. Nothing here depends on who started the call —
//! a call with people in it is live, a call five seconds empty is over.
//!
//! ## What the server does not learn
//!
//! Nothing new: the room id and the connected peers are what the signaling
//! layer already holds (invariant #2). The table is keyed on the room
//! (`BLAKE3_derive_key("lakko/sfu/room/v1", call_id)`, ADR-0078 section 2),
//! never the call id and never an account. The state route takes no token:
//! knowing the room id is the capability, and only the call's members can
//! compute it, so no admission token is minted just to probe and the issuer
//! no longer learns who probes.
//!
//! Pure over an injected `Instant`, so the rule is unit-tested without a
//! socket; the signaling layer feeds it joins and leaves.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a room may stand empty before the call is over. Chosen, not
/// measured: long enough for a socket to blip and come back, short enough
/// that nobody joins a room the last person left a minute ago.
pub const CALL_EMPTY_GRACE: Duration = Duration::from_secs(5);

/// How long an ended call is remembered, so a late join is still refused
/// rather than founding a second call under the same id. The chat-side
/// record never treats a call older than four hours as live, so an id this
/// old cannot be offered to anyone.
pub const ENDED_CALL_RETENTION: Duration = Duration::from_secs(4 * 60 * 60);

/// What the server knows about one room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallStatus {
    /// Never seen, or seen so long ago it was forgotten: a first join founds
    /// it.
    Unknown,
    /// Somebody is connected.
    Live {
        /// How many sockets the call has.
        peers: usize,
    },
    /// Nobody is connected; the grace is running and a join revives it.
    Closing,
    /// The grace ran out with nobody there. Joins are refused.
    Ended,
}

impl CallStatus {
    /// The stable word the state route answers with.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Live { .. } => "live",
            Self::Closing => "closing",
            Self::Ended => "ended",
        }
    }
}

/// The answer to "may this socket join".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Counted in; the call is live.
    Admitted,
    /// The call ended before this socket arrived.
    Ended,
}

#[derive(Debug, Clone, Copy)]
enum Entry {
    Live { peers: usize },
    Closing { since: Instant },
    Ended { at: Instant },
}

/// Shared, per-process lifetime table. Clone freely.
#[derive(Clone, Default)]
pub struct CallLifetimes {
    inner: Arc<Mutex<HashMap<String, Entry>>>,
}

impl CallLifetimes {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A socket for `room` was admitted at `now`. Counts it in unless the
    /// call ended, in which case the caller refuses the socket.
    pub fn admit(&self, room: &str, now: Instant) -> Admission {
        let mut guard = self.lock();
        let settled = Self::settle(guard.get(room).copied(), now);
        match settled {
            Some(Entry::Ended { .. }) => Admission::Ended,
            Some(Entry::Live { peers }) => {
                guard.insert(room.to_owned(), Entry::Live { peers: peers + 1 });
                Admission::Admitted
            },
            Some(Entry::Closing { .. }) | None => {
                guard.insert(room.to_owned(), Entry::Live { peers: 1 });
                Admission::Admitted
            },
        }
    }

    /// A socket for `room` closed at `now`. The last one out starts the
    /// grace.
    pub fn depart(&self, room: &str, now: Instant) {
        let mut guard = self.lock();
        match Self::settle(guard.get(room).copied(), now) {
            Some(Entry::Live { peers }) if peers > 1 => {
                guard.insert(room.to_owned(), Entry::Live { peers: peers - 1 });
            },
            Some(Entry::Live { .. }) => {
                guard.insert(room.to_owned(), Entry::Closing { since: now });
            },
            // A depart for a call that is already closing, ended or unknown
            // is a bookkeeping mismatch; nothing to move.
            Some(Entry::Closing { .. } | Entry::Ended { .. }) | None => {},
        }
    }

    /// What the server knows about `room` at `now`.
    pub fn status(&self, room: &str, now: Instant) -> CallStatus {
        let guard = self.lock();
        match Self::settle(guard.get(room).copied(), now) {
            None => CallStatus::Unknown,
            Some(Entry::Live { peers }) => CallStatus::Live { peers },
            Some(Entry::Closing { .. }) => CallStatus::Closing,
            Some(Entry::Ended { .. }) => CallStatus::Ended,
        }
    }

    /// Forget ended calls older than [`ENDED_CALL_RETENTION`] and settle the
    /// rest. Called on a slow timer; correctness never depends on it, since
    /// every read settles what it reads.
    pub fn sweep(&self, now: Instant) {
        let mut guard = self.lock();
        guard.retain(|_, entry| {
            *entry = match Self::settle(Some(*entry), now) {
                Some(e) => e,
                None => return false,
            };
            true
        });
    }

    /// How many rooms the table holds (tests and diagnostics).
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether the table is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Move an entry along the clock: a closing call whose grace ran out is
    /// ended, an ended call past retention is forgotten (`None`).
    fn settle(entry: Option<Entry>, now: Instant) -> Option<Entry> {
        // Two steps, in order: a closing call whose grace ran out ended at
        // the end of the grace, and an ended call is forgotten once retention
        // has passed since then — which a single read may need both of.
        let entry = match entry {
            Some(Entry::Closing { since }) if now.duration_since(since) >= CALL_EMPTY_GRACE => {
                Some(Entry::Ended {
                    at: since + CALL_EMPTY_GRACE,
                })
            },
            other => other,
        };
        match entry {
            Some(Entry::Ended { at }) if now.duration_since(at) >= ENDED_CALL_RETENTION => None,
            other => other,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        // A poisoned lock means a panic while holding it; the table is plain
        // data, so carry on with whatever it holds rather than take the
        // signaling task down with it.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn a_first_join_founds_a_live_call() {
        let calls = CallLifetimes::new();
        let now = t0();
        assert_eq!(calls.status("c", now), CallStatus::Unknown);
        assert_eq!(calls.admit("c", now), Admission::Admitted);
        assert_eq!(calls.status("c", now), CallStatus::Live { peers: 1 });
    }

    #[test]
    fn the_last_one_out_starts_the_grace_and_the_grace_ends_the_call() {
        let calls = CallLifetimes::new();
        let now = t0();
        calls.admit("c", now);
        calls.admit("c", now);
        calls.depart("c", now + Duration::from_secs(1));
        assert_eq!(
            calls.status("c", now + Duration::from_secs(1)),
            CallStatus::Live { peers: 1 }
        );
        calls.depart("c", now + Duration::from_secs(2));
        assert_eq!(
            calls.status("c", now + Duration::from_secs(2)),
            CallStatus::Closing
        );
        assert_eq!(
            calls.status(
                "c",
                now + Duration::from_secs(2)
                    + CALL_EMPTY_GRACE.saturating_sub(Duration::from_millis(1))
            ),
            CallStatus::Closing
        );
        assert_eq!(
            calls.status("c", now + Duration::from_secs(2) + CALL_EMPTY_GRACE),
            CallStatus::Ended
        );
    }

    #[test]
    fn a_join_inside_the_grace_keeps_the_call_alive() {
        // The bad-connection case: the socket blips and comes back.
        let calls = CallLifetimes::new();
        let now = t0();
        calls.admit("c", now);
        calls.depart("c", now + Duration::from_secs(10));
        let back = now
            + Duration::from_secs(10)
            + CALL_EMPTY_GRACE.saturating_sub(Duration::from_millis(1));
        assert_eq!(calls.admit("c", back), Admission::Admitted);
        assert_eq!(calls.status("c", back), CallStatus::Live { peers: 1 });
        // And the grace starts over from the next departure, not the first.
        calls.depart("c", back + Duration::from_secs(30));
        assert_eq!(
            calls.status("c", back + Duration::from_secs(30) + Duration::from_secs(4)),
            CallStatus::Closing
        );
    }

    #[test]
    fn a_join_after_the_grace_is_refused_and_the_call_stays_ended() {
        let calls = CallLifetimes::new();
        let now = t0();
        calls.admit("c", now);
        calls.depart("c", now + Duration::from_secs(1));
        let late = now + Duration::from_secs(1) + CALL_EMPTY_GRACE;
        assert_eq!(calls.admit("c", late), Admission::Ended);
        assert_eq!(calls.status("c", late), CallStatus::Ended);
        // Refusing did not count the socket in.
        assert_eq!(
            calls.admit("c", late + Duration::from_secs(60)),
            Admission::Ended
        );
    }

    #[test]
    fn an_ended_call_is_forgotten_after_retention_and_a_new_call_may_reuse_nothing_before() {
        let calls = CallLifetimes::new();
        let now = t0();
        calls.admit("c", now);
        calls.depart("c", now);
        let ended_at = now + CALL_EMPTY_GRACE;
        assert_eq!(
            calls.status(
                "c",
                ended_at + ENDED_CALL_RETENTION.saturating_sub(Duration::from_secs(1))
            ),
            CallStatus::Ended
        );
        assert_eq!(
            calls.status("c", ended_at + ENDED_CALL_RETENTION),
            CallStatus::Unknown
        );
        calls.sweep(ended_at + ENDED_CALL_RETENTION);
        assert!(calls.is_empty());
    }

    #[test]
    fn calls_are_independent() {
        let calls = CallLifetimes::new();
        let now = t0();
        calls.admit("a", now);
        calls.admit("b", now);
        calls.depart("a", now);
        let later = now + CALL_EMPTY_GRACE;
        assert_eq!(calls.status("a", later), CallStatus::Ended);
        assert_eq!(calls.status("b", later), CallStatus::Live { peers: 1 });
    }

    #[test]
    fn a_stray_depart_moves_nothing() {
        let calls = CallLifetimes::new();
        let now = t0();
        calls.depart("never", now);
        assert_eq!(calls.status("never", now), CallStatus::Unknown);
        calls.admit("c", now);
        calls.depart("c", now);
        calls.depart("c", now);
        assert_eq!(calls.status("c", now), CallStatus::Closing);
    }
}
