// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Channel content-key seed.
//!
//! The 32-byte seed is the only secret that controls channel
//! confidentiality. Per ADR-0010 § "Tier 2 — Sender-key broadcast"
//! it is derived from the PublisherGroup's MLS exporter with a
//! fixed label and the `channel_id` as context:
//!
//! ```text
//! content_key_seed = mls_exporter("lakko/channel/v1", channel_id, 32)
//! ```
//!
//! This module is **transport-only** — it does not know about
//! OpenMLS. The caller (a publisher that already owns a
//! [`lakko_mls::Group`]) calls `group.export_secret(identity,
//! EXPORTER_LABEL_V1, channel_id, CONTENT_KEY_SEED_LEN)` and feeds
//! the raw bytes into [`ContentKeySeed::from_exporter_output`]. We
//! keep the wrapping here so `lakko-channels` does not pull
//! OpenMLS into its dependency tree (smaller fuzzing surface,
//! cleaner crate graph).

use lakko_crypto::secret::SecretBytes;

use crate::error::{ChannelError, Result};

/// MLS exporter label for the channel content-key seed, frozen at v1.
///
/// Mirrors the constant cited in ADR-0010. Pinned here as a
/// `&'static str` so a future schema bump (v2) is a deliberate
/// search-and-replace, not a typo waiting to happen.
pub const EXPORTER_LABEL_V1: &str = "lakko/channel/v1";

/// Required seed length in bytes (the exporter output the caller
/// must request).
pub const CONTENT_KEY_SEED_LEN: usize = 32;

/// Opaque handle around the 32-byte content-key seed.
///
/// Wraps `SecretBytes<32>` so the bytes are zeroed on drop and
/// equality is constant-time. The raw bytes are intentionally
/// **not** exposed through the public API — every consumer in this
/// crate borrows the handle and reaches in through `as_inner_bytes`
/// (crate-private). This keeps the seed from accidentally landing
/// in a `tracing` field or a Display impl.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentKeySeed(SecretBytes<CONTENT_KEY_SEED_LEN>);

impl ContentKeySeed {
    /// Wrap raw exporter output as a [`ContentKeySeed`].
    ///
    /// # Errors
    ///
    /// Returns [`ChannelError::SeedLength`] if `bytes.len() !=
    /// CONTENT_KEY_SEED_LEN`. The caller's `export_secret` call
    /// must have requested exactly 32 bytes; we double-check
    /// because a transposed-arguments bug (`length` swapped with
    /// `context.len()`) is silent at the type level but
    /// catastrophic at runtime.
    pub fn from_exporter_output(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != CONTENT_KEY_SEED_LEN {
            return Err(ChannelError::SeedLength {
                expected: CONTENT_KEY_SEED_LEN,
                got: bytes.len(),
            });
        }
        let mut buf = [0u8; CONTENT_KEY_SEED_LEN];
        buf.copy_from_slice(bytes);
        Ok(Self(SecretBytes::new(buf)))
    }

    /// Test-only constructor accepting a fixed array. Public so
    /// downstream FFI tests can build a seed without owning an
    /// MLS group. Production code must always go through
    /// [`Self::from_exporter_output`].
    #[must_use]
    pub const fn from_bytes_for_test(bytes: [u8; CONTENT_KEY_SEED_LEN]) -> Self {
        Self(SecretBytes::new(bytes))
    }

    /// Borrow the seed bytes for crate-private use.
    pub(crate) fn as_inner_bytes(&self) -> &[u8; CONTENT_KEY_SEED_LEN] {
        self.0.as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_wrong_length() {
        assert!(ContentKeySeed::from_exporter_output(&[0u8; 31]).is_err());
        assert!(ContentKeySeed::from_exporter_output(&[0u8; 33]).is_err());
        assert!(ContentKeySeed::from_exporter_output(&[]).is_err());
    }

    #[test]
    fn roundtrip_through_exporter_output() {
        let raw = [0xA5u8; CONTENT_KEY_SEED_LEN];
        let seed = ContentKeySeed::from_exporter_output(&raw).unwrap();
        assert_eq!(seed.as_inner_bytes(), &raw);
    }

    #[test]
    fn debug_does_not_leak_seed_bytes() {
        let seed = ContentKeySeed::from_bytes_for_test([0xAA; CONTENT_KEY_SEED_LEN]);
        let dbg = format!("{seed:?}");
        assert!(!dbg.contains("AA"), "Debug must not leak seed: {dbg}");
    }

    #[test]
    fn equality_is_value_based() {
        let a = ContentKeySeed::from_bytes_for_test([1; CONTENT_KEY_SEED_LEN]);
        let b = ContentKeySeed::from_bytes_for_test([1; CONTENT_KEY_SEED_LEN]);
        let c = ContentKeySeed::from_bytes_for_test([2; CONTENT_KEY_SEED_LEN]);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn exporter_label_is_pinned() {
        // Locking this string into the test surface so a typo bump
        // ("lakko/channel/v2") in a refactor lights up loudly.
        assert_eq!(EXPORTER_LABEL_V1, "lakko/channel/v1");
    }
}
