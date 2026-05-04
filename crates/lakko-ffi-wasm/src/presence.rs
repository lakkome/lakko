// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Mirror of `lakko-presence`: blinded per-group presence tokens.

use std::sync::Mutex;

use lakko_crypto::secret::SecretBytes;
use lakko_presence::{
    current_epoch as rust_current_epoch, derive_token as rust_derive_token,
    PresenceToken as RustPresenceToken, PresenceTracker as RustPresenceTracker,
    DEFAULT_PRESENCE_WINDOW_MS, PRESENCE_TOKEN_LEN,
};
use wasm_bindgen::prelude::*;

const SEED_LEN: usize = 32;
const MEMBER_LEN: usize = 32;

/// 16-byte presence token. Mirror of [`lakko_presence::PresenceToken`].
#[wasm_bindgen]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct PresenceToken {
    inner: RustPresenceToken,
}

#[wasm_bindgen]
impl PresenceToken {
    /// Wrap raw bytes (e.g. parsed off a wire format). Must be exactly
    /// 16 bytes.
    #[wasm_bindgen(js_name = fromBytes)]
    pub fn from_bytes(bytes: &[u8]) -> Result<PresenceToken, JsError> {
        if bytes.len() != PRESENCE_TOKEN_LEN {
            return Err(JsError::new(&format!(
                "presence token must be {PRESENCE_TOKEN_LEN} bytes",
            )));
        }
        let mut arr = [0u8; PRESENCE_TOKEN_LEN];
        arr.copy_from_slice(bytes);
        Ok(Self {
            inner: RustPresenceToken::from_bytes(arr),
        })
    }

    /// Return the underlying 16 raw bytes.
    #[wasm_bindgen(js_name = asBytes)]
    pub fn as_bytes(&self) -> Vec<u8> {
        self.inner.as_bytes().to_vec()
    }
}

/// Derive a token for `(presenceSeed, memberPseudonym, epoch)`.
/// Mirror of [`lakko_presence::derive_token`].
#[wasm_bindgen(js_name = deriveToken)]
pub fn derive_token(
    presence_seed: &[u8],
    member_pseudonym: &[u8],
    epoch: u64,
) -> Result<PresenceToken, JsError> {
    if presence_seed.len() != SEED_LEN {
        return Err(JsError::new(&format!(
            "presenceSeed must be {SEED_LEN} bytes",
        )));
    }
    if member_pseudonym.len() != MEMBER_LEN {
        return Err(JsError::new(&format!(
            "memberPseudonym must be {MEMBER_LEN} bytes",
        )));
    }
    let mut seed_bytes = [0u8; SEED_LEN];
    seed_bytes.copy_from_slice(presence_seed);
    let seed = SecretBytes::<SEED_LEN>::new(seed_bytes);

    let mut member_arr = [0u8; MEMBER_LEN];
    member_arr.copy_from_slice(member_pseudonym);

    Ok(PresenceToken {
        inner: rust_derive_token(&seed, &member_arr, epoch),
    })
}

/// Map a UNIX-millisecond timestamp to the current epoch counter.
/// Mirror of [`lakko_presence::current_epoch`].
#[wasm_bindgen(js_name = currentEpoch)]
pub fn current_epoch(now_unix_ms: u64, epoch_secs: u64) -> u64 {
    rust_current_epoch(now_unix_ms, epoch_secs)
}

/// Per-group presence tracker. Mirror of
/// [`lakko_presence::PresenceTracker`].
#[wasm_bindgen]
pub struct PresenceTracker {
    inner: Mutex<RustPresenceTracker>,
}

#[wasm_bindgen]
impl PresenceTracker {
    /// New tracker with custom presence window.
    #[wasm_bindgen(constructor)]
    pub fn new(presence_window_ms: u64) -> Self {
        Self {
            inner: Mutex::new(RustPresenceTracker::new(presence_window_ms)),
        }
    }

    /// New tracker with [`DEFAULT_PRESENCE_WINDOW_MS`] (60s).
    #[wasm_bindgen(js_name = withDefaultWindow)]
    pub fn with_default_window() -> Self {
        Self::new(DEFAULT_PRESENCE_WINDOW_MS)
    }

    /// Record an observation of `token` at wall-clock time `now`.
    #[wasm_bindgen]
    pub fn observe(&self, token: &PresenceToken, now_unix_ms: u64) -> Result<(), JsError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("presence tracker poisoned"))?;
        g.observe(token.inner, now_unix_ms);
        Ok(())
    }

    /// Is `token` within the presence window as of `now`?
    #[wasm_bindgen(js_name = isPresent)]
    pub fn is_present(&self, token: &PresenceToken, now_unix_ms: u64) -> Result<bool, JsError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("presence tracker poisoned"))?;
        Ok(g.is_present(&token.inner, now_unix_ms))
    }

    /// Drop tokens whose `last_seen_ms` is older than the presence
    /// window.
    #[wasm_bindgen(js_name = purgeExpired)]
    pub fn purge_expired(&self, now_unix_ms: u64) -> Result<(), JsError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("presence tracker poisoned"))?;
        g.purge_expired(now_unix_ms);
        Ok(())
    }

    /// How many tokens are currently tracked.
    #[wasm_bindgen(getter)]
    pub fn len(&self) -> Result<usize, JsError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("presence tracker poisoned"))?;
        Ok(g.len())
    }

    /// `true` if no observations recorded.
    #[wasm_bindgen(js_name = isEmpty, getter)]
    pub fn is_empty(&self) -> Result<bool, JsError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("presence tracker poisoned"))?;
        Ok(g.is_empty())
    }
}

impl Default for PresenceTracker {
    fn default() -> Self {
        Self::with_default_window()
    }
}

// Happy-path tests only. Error-path validation invokes `JsError::new`,
// which wasm-bindgen reroutes through a JS-imported stub that panics
// on host targets — coverage lives in the `lakko-presence` crate.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_from_bytes_roundtrip() {
        let t = PresenceToken::from_bytes(&[0xABu8; 16]).unwrap();
        assert_eq!(t.as_bytes(), vec![0xABu8; 16]);
    }

    #[test]
    fn derive_token_is_deterministic() {
        let seed = [0xAAu8; 32];
        let member = [0x11u8; 32];
        let a = derive_token(&seed, &member, 100).unwrap();
        let b = derive_token(&seed, &member, 100).unwrap();
        assert_eq!(a.as_bytes(), b.as_bytes());
    }

    #[test]
    fn current_epoch_chunks() {
        assert_eq!(current_epoch(0, 60), 0);
        assert_eq!(current_epoch(60_000, 60), 1);
    }

    #[test]
    fn presence_tracker_observe_and_check() {
        let t = PresenceTracker::new(60_000);
        let token = PresenceToken::from_bytes(&[1u8; 16]).unwrap();
        t.observe(&token, 1_000_000).unwrap();
        assert!(t.is_present(&token, 1_000_000).unwrap());
        assert!(!t.is_present(&token, 1_100_000).unwrap());
    }

    #[test]
    fn presence_tracker_purge() {
        let t = PresenceTracker::new(60_000);
        let stale = PresenceToken::from_bytes(&[1u8; 16]).unwrap();
        let fresh = PresenceToken::from_bytes(&[2u8; 16]).unwrap();
        t.observe(&stale, 0).unwrap();
        t.observe(&fresh, 100_000).unwrap();
        t.purge_expired(120_000).unwrap();
        assert_eq!(t.len().unwrap(), 1);
    }
}
