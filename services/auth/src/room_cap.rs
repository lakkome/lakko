// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Per-room admission cap for `POST /call/jwt` (ADR-0078 section 2, the
//! rebuilt ADR-0033 throttle).
//!
//! ADR-0033 counted distinct fresh *accounts* per `call_id` in a table
//! (`call_admission_events (call_id, account_id, at)`), which put an account
//! next to every call it joined. Under ADR-0078 the mint takes no session,
//! so the issuer does not know the account at all; the "fresh account"
//! signal moved to issuance (young accounts get a small `call-join` quota,
//! `anon_tokens.rs`), and what is left here is a cap on how many admission
//! tokens one room may be issued per window, with no account anywhere.
//!
//! The cap lives in memory only: a room id is a one-way function of a call
//! id, it is never written to a table or a log, and the map forgets a room
//! once its window has passed. One auth process serves the deployment, so
//! a process-local count is the count. A restart forgets every count, which
//! errs towards admitting.
//!
//! The cap must admit genuine large calls (every participant mints at join
//! and again at a reconnect), so its default is generous; a burst of fresh
//! accounts can fill a room up to it (the cost ADR-0078 decision 2
//! accepts). Configuration:
//!
//! - `LAKKO_AUTH_ROOM_ADMISSION_LIMIT` - admissions per room per window,
//!   default 100 (chosen, not measured); `0` disables the cap.
//! - `LAKKO_AUTH_ROOM_ADMISSION_WINDOW_SECS` - the sliding window, default
//!   60.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Env var: admissions per room per window (`0` disables).
const LIMIT_VAR: &str = "LAKKO_AUTH_ROOM_ADMISSION_LIMIT";
/// Env var: the sliding window in seconds.
const WINDOW_VAR: &str = "LAKKO_AUTH_ROOM_ADMISSION_WINDOW_SECS";
/// Default admissions per room per window. Chosen, not measured.
const DEFAULT_LIMIT: u32 = 100;
/// Default window.
const DEFAULT_WINDOW_SECS: u64 = 60;

/// The answer to one admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoomAdmission {
    /// Counted in; mint the token.
    Admitted,
    /// The room had its window's worth of admissions; refuse (`429`).
    Full,
}

/// The in-memory per-room cap.
#[derive(Debug)]
pub(crate) struct RoomAdmissionCap {
    limit: u32,
    window: Duration,
    state: Mutex<CapState>,
}

#[derive(Debug, Default)]
struct CapState {
    rooms: HashMap<[u8; 32], VecDeque<Instant>>,
    last_sweep: Option<Instant>,
}

impl RoomAdmissionCap {
    /// A cap of `limit` admissions per `window`; `limit == 0` disables it.
    pub(crate) fn new(limit: u32, window: Duration) -> Self {
        Self {
            limit,
            window,
            state: Mutex::new(CapState::default()),
        }
    }

    /// A disabled cap (tests and unwired builders).
    #[cfg(test)]
    pub(crate) fn disabled() -> Self {
        Self::new(0, Duration::from_secs(DEFAULT_WINDOW_SECS))
    }

    /// Load from the environment.
    ///
    /// # Errors
    ///
    /// A value that is not a non-negative integer, or a zero window.
    pub(crate) fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|v| std::env::var(v).ok())
    }

    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        use anyhow::Context as _;
        let limit = match lookup(LIMIT_VAR).map(|s| s.trim().to_owned()) {
            Some(s) if !s.is_empty() => s
                .parse::<u32>()
                .with_context(|| format!("{LIMIT_VAR} must be a non-negative integer"))?,
            _ => DEFAULT_LIMIT,
        };
        let window_secs = match lookup(WINDOW_VAR).map(|s| s.trim().to_owned()) {
            Some(s) if !s.is_empty() => s
                .parse::<u64>()
                .with_context(|| format!("{WINDOW_VAR} must be a positive integer"))?,
            _ => DEFAULT_WINDOW_SECS,
        };
        if window_secs == 0 {
            anyhow::bail!("{WINDOW_VAR} must be positive");
        }
        Ok(Self::new(limit, Duration::from_secs(window_secs)))
    }

    /// Log the configuration at startup.
    pub(crate) fn log_startup(&self) {
        if self.limit == 0 {
            tracing::info!("per-room call admission cap disabled ({LIMIT_VAR}=0)");
        } else {
            tracing::info!(
                limit = self.limit,
                window_secs = self.window.as_secs(),
                "per-room call admission cap enabled"
            );
        }
    }

    /// Count one admission to `room` at `now`, or refuse when the room had
    /// `limit` admissions inside the window. A poisoned lock is recovered:
    /// the map holds counts only.
    pub(crate) fn try_admit(&self, room: &[u8; 32], now: Instant) -> RoomAdmission {
        if self.limit == 0 {
            return RoomAdmission::Admitted;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let window = self.window;
        // Forget rooms whose whole window has passed, at most once a window.
        if state
            .last_sweep
            .map_or(true, |t| now.saturating_duration_since(t) >= window)
        {
            state.rooms.retain(|_, times| {
                times
                    .back()
                    .is_some_and(|t| now.saturating_duration_since(*t) < window)
            });
            state.last_sweep = Some(now);
        }
        let times = state.rooms.entry(*room).or_default();
        while times
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= window)
        {
            times.pop_front();
        }
        if times.len() >= usize::try_from(self.limit).unwrap_or(usize::MAX) {
            return RoomAdmission::Full;
        }
        times.push_back(now);
        RoomAdmission::Admitted
    }

    /// How many rooms the map holds (tests).
    #[cfg(test)]
    fn rooms(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .rooms
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_room_is_capped_per_window_and_reopens_after_it() {
        let cap = RoomAdmissionCap::new(3, Duration::from_secs(60));
        let t0 = Instant::now();
        let room = [7u8; 32];
        for i in 0..3 {
            assert_eq!(
                cap.try_admit(&room, t0 + Duration::from_secs(i)),
                RoomAdmission::Admitted
            );
        }
        assert_eq!(
            cap.try_admit(&room, t0 + Duration::from_secs(10)),
            RoomAdmission::Full
        );
        // Another room is counted on its own.
        assert_eq!(cap.try_admit(&[8u8; 32], t0), RoomAdmission::Admitted);
        // Once the first admission leaves the window, one more fits.
        assert_eq!(
            cap.try_admit(&room, t0 + Duration::from_secs(60)),
            RoomAdmission::Admitted
        );
    }

    #[test]
    fn a_disabled_cap_admits_everything() {
        let cap = RoomAdmissionCap::disabled();
        let t0 = Instant::now();
        for _ in 0..1000 {
            assert_eq!(cap.try_admit(&[1u8; 32], t0), RoomAdmission::Admitted);
        }
        assert_eq!(cap.rooms(), 0);
    }

    #[test]
    fn idle_rooms_are_forgotten() {
        let cap = RoomAdmissionCap::new(5, Duration::from_secs(60));
        let t0 = Instant::now();
        cap.try_admit(&[1u8; 32], t0);
        cap.try_admit(&[2u8; 32], t0);
        assert_eq!(cap.rooms(), 2);
        cap.try_admit(&[3u8; 32], t0 + Duration::from_secs(61));
        assert_eq!(cap.rooms(), 1);
    }

    #[test]
    fn configuration_parses_and_refuses_garbage() {
        let cap = RoomAdmissionCap::from_lookup(|_| None).expect("defaults");
        assert_eq!(cap.limit, DEFAULT_LIMIT);
        assert_eq!(cap.window, Duration::from_secs(DEFAULT_WINDOW_SECS));
        let cap = RoomAdmissionCap::from_lookup(|v| match v {
            LIMIT_VAR => Some("0".into()),
            _ => None,
        })
        .expect("disabled");
        assert_eq!(cap.limit, 0);
        assert!(RoomAdmissionCap::from_lookup(|v| match v {
            LIMIT_VAR => Some("-1".into()),
            _ => None,
        })
        .is_err());
        assert!(RoomAdmissionCap::from_lookup(|v| match v {
            WINDOW_VAR => Some("0".into()),
            _ => None,
        })
        .is_err());
    }
}
