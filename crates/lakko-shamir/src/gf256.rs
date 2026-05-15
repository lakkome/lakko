// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! GF(2^8) arithmetic over the AES Rijndael field.
//!
//! Reducing polynomial: `x^8 + x^4 + x^3 + x + 1` (the standard
//! AES polynomial, byte form `0x11B`). Multiplication uses
//! pre-computed log / exp tables for a primitive element `g = 3`
//! (the same generator AES picks); the tables collapse the
//! per-byte multiply to two table lookups + an add.
//!
//! Addition and subtraction are both XOR (characteristic 2).
//! Division uses the multiplicative inverse computed via the log
//! tables: `a / b = exp[(log[a] - log[b]) mod 255]`.
//!
//! ## Performance / side-channel posture
//!
//! Lookup-based GF(256) is fast (two table reads + a wrapping
//! add per multiply) but NOT constant-time — different inputs
//! hit different cache lines. For the social-recovery flow the
//! adversary needs to observe in-process cache timing to
//! exploit this, which is well outside Lakko's Phase-1 threat
//! model (Yarovaya / IPA-style legal compulsion + server-side
//! ciphertext access, not local-process side channels). If we
//! ever ship a Shamir flow that runs inside a TEE alongside
//! attacker code, we swap to a bitsliced constant-time variant.
//!
//! Tables are built lazily at first use via `OnceLock` to avoid
//! a `lazy_static!` / `once_cell` dependency.

use std::sync::OnceLock;

/// Add in GF(2^8) — same as subtract, both are XOR.
#[inline]
pub(crate) fn add(a: u8, b: u8) -> u8 {
    a ^ b
}

/// Multiply in GF(2^8). Returns `0` when either operand is `0`;
/// otherwise routes through the log / exp tables.
#[inline]
pub(crate) fn mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        return 0;
    }
    let (log_t, exp_t) = tables();
    let la = u16::from(log_t[a as usize]);
    let lb = u16::from(log_t[b as usize]);
    // Sum modulo 255 — the multiplicative group has order 255.
    let s = (la + lb) % 255;
    exp_t[s as usize]
}

/// Divide in GF(2^8). Panics when `b == 0` — by construction
/// (every call site comes from the Lagrange basis denominator,
/// where the duplicate-index check already filters the only
/// way `b` would be zero).
///
/// Implementation: `a / b = a * b^{-1} = exp[log[a] - log[b]
/// mod 255]`.
#[inline]
pub(crate) fn div(a: u8, b: u8) -> u8 {
    debug_assert!(b != 0, "GF(256) division by zero");
    if a == 0 {
        return 0;
    }
    let (log_t, exp_t) = tables();
    let la = i16::from(log_t[a as usize]);
    let lb = i16::from(log_t[b as usize]);
    // (la - lb) modulo 255 in Z — pull into 0..255 range.
    let s = ((la - lb).rem_euclid(255)) as u16;
    exp_t[s as usize]
}

/// Lazily-built `(LOG, EXP)` table pair.
///
/// `LOG[a]` is `log_g(a)` for `a != 0` (the `LOG[0]` entry is
/// `0`, a placeholder — the multiply and divide fast paths
/// reject zero operands before reading `LOG`).
///
/// `EXP[i]` is `g^i mod p(x)` for `i` in `0..510`. The 2× length
/// lets us skip the modulo on `(la + lb)` for the multiply path
/// when we want — currently we modulo explicitly for clarity,
/// but the table size leaves room for the optimisation.
/// Co-allocated log + exp tables — boxed to keep the function
/// signatures slim, paired to give `tables()` a single
/// `OnceLock` slot. The type alias keeps `clippy::type_complexity`
/// quiet on the `OnceLock` slot below.
type GfTables = (Box<[u8; 256]>, Box<[u8; 256]>);

fn tables() -> &'static GfTables {
    static TABLES: OnceLock<GfTables> = OnceLock::new();
    TABLES.get_or_init(build_tables)
}

fn build_tables() -> GfTables {
    // Generator g = 3. The Rijndael field has g = 3 as a
    // primitive element — every non-zero byte is g^i for some
    // i in 0..255.
    //
    // `xtimes` (a polynomial-multiply by `x`) is the standard
    // AES helper: shift left + conditionally XOR `0x1B` if the
    // high bit was set (the reduction modulo `x^8 + x^4 +
    // x^3 + x + 1`, with `0x1B` being the low 8 bits of the
    // reducing polynomial).
    fn xtimes(a: u8) -> u8 {
        let shifted = a.wrapping_shl(1);
        if (a & 0x80) != 0 {
            shifted ^ 0x1B
        } else {
            shifted
        }
    }
    // Polynomial-multiply: `a * b` in GF(2^8), reducible.
    // Used only at table-build time; the fast paths above go
    // through the log tables instead.
    fn poly_mul(a: u8, b: u8) -> u8 {
        let mut acc: u8 = 0;
        let mut a = a;
        let mut b = b;
        while b != 0 {
            if (b & 1) != 0 {
                acc ^= a;
            }
            a = xtimes(a);
            b >>= 1;
        }
        acc
    }

    let mut exp = Box::new([0u8; 256]);
    let mut log = Box::new([0u8; 256]);
    let mut x: u8 = 1;
    // Build the exp table by iterating g^0, g^1, …, g^254. The
    // 256th entry wraps back to g^0; we leave the last slot at
    // whatever the loop produces (g^255 == g^0 == 1) so a stray
    // read on EXP[255] still returns a valid byte.
    for i in 0..255u8 {
        exp[usize::from(i)] = x;
        log[usize::from(x)] = i;
        x = poly_mul(x, 3);
    }
    // EXP[255] = g^255 = 1 (the multiplicative group cycles).
    exp[255] = 1;
    (log, exp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_is_xor() {
        assert_eq!(add(0, 0), 0);
        assert_eq!(add(0x5A, 0xA5), 0xFF);
        assert_eq!(add(0xFF, 0xFF), 0);
        // Adding the same byte twice yields zero (characteristic 2).
        for a in 0u8..=255 {
            assert_eq!(add(add(a, a), 0), 0);
        }
    }

    #[test]
    fn mul_by_zero_is_zero() {
        for a in 0u8..=255 {
            assert_eq!(mul(a, 0), 0);
            assert_eq!(mul(0, a), 0);
        }
    }

    #[test]
    fn mul_by_one_is_identity() {
        for a in 0u8..=255 {
            assert_eq!(mul(a, 1), a);
            assert_eq!(mul(1, a), a);
        }
    }

    #[test]
    fn mul_is_commutative_and_associative() {
        // Sampled — full 256^3 sweep is overkill for a smoke
        // test. The log-table mechanism makes both properties
        // structural.
        let samples = [0x00, 0x01, 0x02, 0x03, 0x05, 0x10, 0x53, 0x7F, 0xCA, 0xFF];
        for &a in &samples {
            for &b in &samples {
                assert_eq!(mul(a, b), mul(b, a));
                for &c in &samples {
                    assert_eq!(mul(mul(a, b), c), mul(a, mul(b, c)));
                }
            }
        }
    }

    #[test]
    fn div_is_inverse_of_mul() {
        // For all non-zero a, b: (a * b) / b == a.
        for a in 1u8..=255 {
            for b in 1u8..=255 {
                let ab = mul(a, b);
                if ab == 0 {
                    continue;
                }
                assert_eq!(div(ab, b), a, "a={a}, b={b}, a*b={ab}");
            }
        }
    }

    #[test]
    fn every_nonzero_byte_has_an_inverse() {
        for a in 1u8..=255 {
            let inv = div(1, a);
            assert_eq!(mul(a, inv), 1, "a={a}, inv={inv}");
        }
    }

    #[test]
    fn exp_log_round_trip() {
        let (log, exp) = tables();
        // For every nonzero a: exp[log[a]] == a.
        for a in 1u8..=255 {
            assert_eq!(exp[log[a as usize] as usize], a, "a={a}");
        }
    }
}
