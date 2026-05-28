// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! REALITY-aware `SecureRandom` wrapper for rustls.
//!
//! ## Purpose
//!
//! rustls 0.23's `ClientConnection::new` calls
//! `CryptoProvider::secure_random.fill` in this order
//! (verified in `rustls/src/client/hs.rs`):
//!
//! 1. `SessionId::random(secure_random)` → **first 32-byte fill** →
//!    `legacy_session_id` ← REALITY v2 authenticator goes here
//! 2. `random_u16(secure_random)` → 2-byte fill (irrelevant to REALITY)
//! 3. `Random::new(secure_random)` → second 32-byte fill →
//!    `ClientHello.Random` (ring-generated; irrelevant to REALITY)
//!
//! The naïve approach — let rustls generate random bytes, then patch
//! the on-wire `legacy_session_id` before writing to the socket — is
//! incorrect for TLS 1.3: `legacy_session_id` participates in the
//! transcript hash (RFC 8446 §4.4.1), so the two endpoints would
//! derive divergent keys and the first application record would fail
//! to decrypt (`Tls("cannot decrypt peer's message")`).
//!
//! The fix: inject our bytes BEFORE rustls bakes them into the
//! transcript. A stateful `SecureRandom` wrapper intercepts the
//! **first 32-byte fill** and substitutes the REALITY authenticator;
//! all other fills delegate to ring's `SystemRandom`.
//!
//! The same wrapper is used on the server side: the server arms it
//! with `compute_server_response` bytes immediately before
//! `ServerConnection::process_new_packets` processes the inbound
//! `ClientHello` — that processing generates `ServerHello.Random`
//! via the first 32-byte `fill` call on the server path, which
//! receives the server-response HMAC bytes.
//!
//! ## Thread-safety contract
//!
//! The inject slot is a `thread_local!` `Cell` and is consumed by the
//! first `fill(32)` call on this thread. There MUST be no `.await`
//! between [`RealityRng::arm`] and the rustls call that triggers the
//! fill (either `ClientConnection::new` or
//! `ServerConnection::process_new_packets`). This invariant is
//! enforced by keeping both calls together in the same synchronous
//! helper:
//!
//! - Client: `driver::build_reality_client_connection`
//! - Server: the `!server_rng_armed` guard block in
//!   `server_driver::drive_server_handshake` (sync, inside a
//!   non-async block between two await points)

use std::cell::Cell;
use std::fmt;

use rustls::crypto::{GetRandomFailed, SecureRandom};

thread_local! {
    /// One-shot 32-byte payload injected on the first
    /// `fill(32)` call on this thread. Cleared after use.
    static REALITY_INJECT: Cell<Option<[u8; 32]>> = const { Cell::new(None) };
}

/// A `SecureRandom` implementation that injects a
/// caller-provided 32-byte payload into exactly one
/// `fill(32)` call, then delegates all other calls to
/// ring's `SystemRandom`.
///
/// The singleton [`REALITY_RNG`] is used as the
/// `secure_random` field of the per-connection
/// `CryptoProvider`. Arm the inject slot via
/// [`RealityRng::arm`] immediately before the rustls call
/// that will consume it.
#[derive(Clone, Copy)]
pub(crate) struct RealityRng;

impl fmt::Debug for RealityRng {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RealityRng")
    }
}

/// Global singleton — `'static` so it can be used as
/// `&'static dyn SecureRandom` inside a `CryptoProvider`.
pub(crate) static REALITY_RNG: RealityRng = RealityRng;

impl RealityRng {
    /// Set the one-shot inject payload for this thread.
    ///
    /// MUST be called **immediately before** the rustls
    /// call that will trigger the first `fill(32)` on
    /// this thread. No `.await` must intervene.
    pub(crate) fn arm(bytes: [u8; 32]) {
        REALITY_INJECT.with(|cell| cell.set(Some(bytes)));
    }
}

impl SecureRandom for RealityRng {
    fn fill(&self, buf: &mut [u8]) -> Result<(), GetRandomFailed> {
        // Only intercept exactly-32-byte fills. The 2-byte
        // `random_u16` call and any other size delegate
        // straight to ring, preserving the inject slot.
        if buf.len() == 32 {
            let injected = REALITY_INJECT.with(Cell::take);
            if let Some(bytes) = injected {
                buf.copy_from_slice(&bytes);
                return Ok(());
            }
        }
        // Delegate to ring for the 2-byte random_u16 call,
        // the second 32-byte fill (ClientHello.Random or
        // ServerHello.Random when no inject is set), and
        // any other size rustls might request.
        //
        // `ring::default_provider().secure_random` is a
        // `&'static` reference to ring's SystemRandom
        // singleton; calling `default_provider()` is cheap
        // (stack allocation, no heap).
        rustls::crypto::ring::default_provider()
            .secure_random
            .fill(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Clear the inject slot so tests running on the same
    /// thread do not interfere with each other.
    fn clear_inject() {
        REALITY_INJECT.with(|cell| cell.set(None));
    }

    #[test]
    fn inject_on_first_32_byte_fill() {
        clear_inject();
        let payload = [0xAAu8; 32];
        RealityRng::arm(payload);
        let mut buf = [0u8; 32];
        REALITY_RNG.fill(&mut buf).expect("fill succeeds");
        assert_eq!(buf, payload, "first 32-byte fill must return the armed payload");
    }

    #[test]
    fn inject_consumed_after_first_use() {
        clear_inject();
        let payload = [0xBBu8; 32];
        RealityRng::arm(payload);

        let mut buf1 = [0u8; 32];
        REALITY_RNG.fill(&mut buf1).expect("first fill");
        assert_eq!(buf1, payload);

        let mut buf2 = [0u8; 32];
        REALITY_RNG.fill(&mut buf2).expect("second fill");
        assert_ne!(buf2, payload, "inject must be consumed after first use");
    }

    #[test]
    fn no_inject_when_not_armed() {
        clear_inject();
        let mut buf = [0u8; 32];
        // Must not panic — delegates to ring.
        REALITY_RNG.fill(&mut buf).expect("fill succeeds without inject");
    }

    #[test]
    fn two_byte_fill_bypasses_inject() {
        clear_inject();
        let payload = [0xCCu8; 32];
        RealityRng::arm(payload);

        // The 2-byte `random_u16` call must NOT consume the slot.
        let mut small = [0u8; 2];
        REALITY_RNG.fill(&mut small).expect("2-byte fill");

        // Slot must still be armed.
        let mut buf32 = [0u8; 32];
        REALITY_RNG.fill(&mut buf32).expect("32-byte fill after 2-byte");
        assert_eq!(buf32, payload, "inject must survive a 2-byte fill");
    }

    #[test]
    fn non_32_fill_bypasses_inject() {
        clear_inject();
        let payload = [0xDDu8; 32];
        RealityRng::arm(payload);

        let mut buf16 = [0u8; 16];
        REALITY_RNG.fill(&mut buf16).expect("16-byte fill");

        let mut buf32 = [0u8; 32];
        REALITY_RNG.fill(&mut buf32).expect("32-byte fill after 16-byte");
        assert_eq!(buf32, payload, "inject must survive a 16-byte fill");
    }

    #[test]
    fn arm_overwrites_previous_arm() {
        clear_inject();
        RealityRng::arm([0x11u8; 32]);
        RealityRng::arm([0x22u8; 32]);

        let mut buf = [0u8; 32];
        REALITY_RNG.fill(&mut buf).expect("fill");
        assert_eq!(buf, [0x22u8; 32], "second arm must overwrite first");
    }
}
