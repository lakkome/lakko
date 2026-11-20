// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The issuer key: one per period, derived from a master secret shared by the
//! issuer (`services/auth`) and the verifier (`services/channels`), and the
//! period arithmetic.
//!
//! There is no key table. As with `lakko-anon-token`, every period's key is a
//! deterministic function of the master secret, so the issuer and the verifier
//! derive the same key without coordination and the key changes every period.

use curve25519_dalek::ristretto::RistrettoPoint;
use curve25519_dalek::scalar::Scalar;
use zeroize::{Zeroize, Zeroizing};

use crate::error::{AnonCredError, Result};
use crate::group::{decode_nonidentity_point, generator_g, generator_h};
use crate::transcript::Transcript;
use crate::WIRE_VERSION;

/// Length of the master secret.
pub const MASTER_SECRET_LEN: usize = 32;

/// Length of an encoded issuer public key (`C_x0 || X1 || X2`).
pub const PUBLIC_KEY_LEN: usize = 96;

/// One credential period: 30 days. Chosen, not measured (ADR-0092 4.2).
///
/// Every issuer and verifier must use the same value: the period number is
/// `floor(unix_secs / PERIOD_SECS)`, so a mismatch makes every presentation
/// fail. Changing it renumbers the periods and invalidates every credential.
pub const PERIOD_SECS: u64 = 30 * 86_400;

/// BLAKE3 derive-key context of the per-period issuer key scalars.
const KEY_CONTEXT: &str = "lakko/anon-cred/v1 issuer-key";

/// The period a Unix time (seconds) falls in.
#[must_use]
pub const fn period_at(unix_secs: u64) -> u64 {
    unix_secs / PERIOD_SECS
}

/// Whether a credential for `presented` is accepted in `current`: its own
/// period and the next one (ADR-0092 4.2, the grace at a boundary). A
/// credential for a future period is refused.
#[must_use]
pub const fn period_accepted(presented: u64, current: u64) -> bool {
    presented == current || (presented < u64::MAX && presented + 1 == current)
}

/// The 32-byte secret the issuer and the verifier share. It is separate from
/// the anonymous-token master (`lakko-anon-token`), which other services hold.
///
/// Wiped on drop; `Debug` prints nothing of the value.
#[derive(Clone)]
pub struct MasterSecret(Zeroizing<[u8; MASTER_SECRET_LEN]>);

impl core::fmt::Debug for MasterSecret {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("MasterSecret(..)")
    }
}

impl MasterSecret {
    /// Wrap 32 raw bytes.
    #[must_use]
    pub fn from_bytes(bytes: [u8; MASTER_SECRET_LEN]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// Parse 64 hex characters (either case, surrounding whitespace trimmed).
    ///
    /// # Errors
    ///
    /// [`AnonCredError::InvalidMasterSecret`] for any other length or a
    /// non-hex character.
    pub fn from_hex(hex: &str) -> Result<Self> {
        let hex = hex.trim().as_bytes();
        if hex.len() != MASTER_SECRET_LEN * 2 {
            return Err(AnonCredError::InvalidMasterSecret);
        }
        let mut out = Zeroizing::new([0u8; MASTER_SECRET_LEN]);
        for (i, pair) in hex.chunks_exact(2).enumerate() {
            let hi = hex_val(pair[0]).ok_or(AnonCredError::InvalidMasterSecret)?;
            let lo = hex_val(pair[1]).ok_or(AnonCredError::InvalidMasterSecret)?;
            out[i] = (hi << 4) | lo;
        }
        Ok(Self(out))
    }
}

const fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// The issuer's public key for one period: `C_x0 = x0*G + x0t*H`,
/// `X1 = x1*H`, `X2 = x2*H` (CMZ14 `iparams`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IssuerPublicKey {
    period: u64,
    pub(crate) c_x0: RistrettoPoint,
    pub(crate) x1: RistrettoPoint,
    pub(crate) x2: RistrettoPoint,
}

impl IssuerPublicKey {
    /// The period this key is for.
    #[must_use]
    pub const fn period(&self) -> u64 {
        self.period
    }

    /// `C_x0 || X1 || X2`, 96 bytes. The period is not part of the encoding:
    /// the key is published under its period (the route names it).
    #[must_use]
    pub fn to_bytes(&self) -> [u8; PUBLIC_KEY_LEN] {
        let mut out = [0u8; PUBLIC_KEY_LEN];
        out[..32].copy_from_slice(self.c_x0.compress().as_bytes());
        out[32..64].copy_from_slice(self.x1.compress().as_bytes());
        out[64..].copy_from_slice(self.x2.compress().as_bytes());
        out
    }

    /// Decode the key published for `period`. Every point must be canonical
    /// and none may be the identity.
    ///
    /// # Errors
    ///
    /// `Malformed`, `NonCanonical` or `IdentityPoint`.
    pub fn decode(period: u64, bytes: &[u8]) -> Result<Self> {
        if bytes.len() != PUBLIC_KEY_LEN {
            return Err(AnonCredError::Malformed);
        }
        Ok(Self {
            period,
            c_x0: decode_nonidentity_point(&bytes[..32])?,
            x1: decode_nonidentity_point(&bytes[32..64])?,
            x2: decode_nonidentity_point(&bytes[64..])?,
        })
    }

    /// Bind the key and its period into a transcript.
    pub(crate) fn bind(&self, transcript: &mut Transcript) {
        transcript.append_u64(b"period", self.period);
        transcript.append_point(b"issuer-c-x0", &self.c_x0);
        transcript.append_point(b"issuer-x1", &self.x1);
        transcript.append_point(b"issuer-x2", &self.x2);
    }
}

/// The issuer's secret key for one period: `(x0, x0t, x1, x2)` (CMZ14 `sk`
/// for two attributes: `x1` for the hidden secret `s`, `x2` for the public
/// period). Wiped on drop.
pub struct IssuerSecretKey {
    period: u64,
    pub(crate) x0: Scalar,
    pub(crate) x0t: Scalar,
    pub(crate) x1: Scalar,
    pub(crate) x2: Scalar,
    public: IssuerPublicKey,
}

impl Drop for IssuerSecretKey {
    fn drop(&mut self) {
        self.x0.zeroize();
        self.x0t.zeroize();
        self.x1.zeroize();
        self.x2.zeroize();
    }
}

impl core::fmt::Debug for IssuerSecretKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("IssuerSecretKey")
            .field("period", &self.period)
            .finish_non_exhaustive()
    }
}

impl IssuerSecretKey {
    /// Derive the key for `period` from the master secret.
    ///
    /// Each scalar is `BLAKE3-derive-key("lakko/anon-cred/v1 issuer-key",
    /// master || version || period_be || name)` in XOF mode, 64 bytes, reduced
    /// modulo the group order, for `name` in `x0`, `x0t`, `x1`, `x2`.
    ///
    /// # Errors
    ///
    /// [`AnonCredError::ZeroScalar`] if a derived scalar is zero (probability
    /// about 2^-250 per key).
    pub fn derive(master: &MasterSecret, period: u64) -> Result<Self> {
        let scalar = |name: &[u8]| -> Result<Scalar> {
            let mut hasher = blake3::Hasher::new_derive_key(KEY_CONTEXT);
            hasher.update(master.0.as_ref());
            hasher.update(&[WIRE_VERSION]);
            hasher.update(&period.to_be_bytes());
            hasher.update(name);
            let mut wide = Zeroizing::new([0u8; 64]);
            hasher.finalize_xof().fill(wide.as_mut());
            let s = Scalar::from_bytes_mod_order_wide(&wide);
            if s == Scalar::ZERO {
                return Err(AnonCredError::ZeroScalar);
            }
            Ok(s)
        };
        let x0 = scalar(b"x0")?;
        let x0t = scalar(b"x0t")?;
        let x1 = scalar(b"x1")?;
        let x2 = scalar(b"x2")?;
        let g = generator_g();
        let h = generator_h();
        let public = IssuerPublicKey {
            period,
            c_x0: x0 * g + x0t * h,
            x1: x1 * h,
            x2: x2 * h,
        };
        Ok(Self {
            period,
            x0,
            x0t,
            x1,
            x2,
            public,
        })
    }

    /// The period this key is for.
    #[must_use]
    pub const fn period(&self) -> u64 {
        self.period
    }

    /// The public key a client fetches for this period.
    #[must_use]
    pub const fn public_key(&self) -> IssuerPublicKey {
        self.public
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn master(b: u8) -> MasterSecret {
        MasterSecret::from_bytes([b; 32])
    }

    #[test]
    fn derivation_is_deterministic_and_separated() {
        let a = IssuerSecretKey::derive(&master(1), 10)
            .unwrap()
            .public_key();
        let b = IssuerSecretKey::derive(&master(1), 10)
            .unwrap()
            .public_key();
        let c = IssuerSecretKey::derive(&master(1), 11)
            .unwrap()
            .public_key();
        let d = IssuerSecretKey::derive(&master(2), 10)
            .unwrap()
            .public_key();
        assert_eq!(a, b);
        assert_ne!(a.to_bytes(), c.to_bytes());
        assert_ne!(a.to_bytes(), d.to_bytes());
    }

    #[test]
    fn public_key_round_trip() {
        let key = IssuerSecretKey::derive(&master(3), 5).unwrap().public_key();
        assert_eq!(IssuerPublicKey::decode(5, &key.to_bytes()).unwrap(), key);
        assert_eq!(
            IssuerPublicKey::decode(5, &[0u8; 95]),
            Err(AnonCredError::Malformed)
        );
        assert_eq!(
            IssuerPublicKey::decode(5, &[0u8; 96]),
            Err(AnonCredError::IdentityPoint)
        );
    }

    #[test]
    fn periods() {
        assert_eq!(period_at(0), 0);
        assert_eq!(period_at(PERIOD_SECS - 1), 0);
        assert_eq!(period_at(PERIOD_SECS), 1);
        assert!(period_accepted(4, 4));
        assert!(period_accepted(3, 4));
        assert!(!period_accepted(2, 4));
        assert!(!period_accepted(5, 4));
        assert!(!period_accepted(u64::MAX, 0));
    }

    #[test]
    fn master_secret_hex() {
        assert!(MasterSecret::from_hex(&"ab".repeat(32)).is_ok());
        assert!(MasterSecret::from_hex(&format!(" {} ", "AB".repeat(32))).is_ok());
        assert!(MasterSecret::from_hex(&"ab".repeat(31)).is_err());
        assert!(MasterSecret::from_hex(&"zz".repeat(32)).is_err());
    }
}
