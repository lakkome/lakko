// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Measures proof size and the time to cast and to verify one ballot for a
//! few ring sizes. Run with `cargo run --release -p lakko-ballot --example
//! measure [sizes...]`. One run, wall clock, no warm-up beyond one cast: a
//! single measurement, not a benchmark.

use std::io::Write;
use std::time::Instant;

use lakko_ballot::ballot::open_row;
use lakko_ballot::{cast, open_and_verify, Choices, PollKeys, Ring, VoterKey};

fn seed(i: u32) -> [u8; 32] {
    let mut s = [0u8; 32];
    s[..4].copy_from_slice(&i.to_be_bytes());
    s[4] = 1;
    s
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sizes: Vec<u32> = {
        let args: Vec<u32> = std::env::args()
            .skip(1)
            .filter_map(|a| a.parse().ok())
            .collect();
        if args.is_empty() {
            vec![16, 256, 4096, 65_536, 262_144]
        } else {
            args
        }
    };
    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "| ring | digits | proof bytes | row bytes | ring build ms | cast ms | open+verify ms |"
    )?;
    writeln!(out, "|---|---|---|---|---|---|---|")?;
    for n in sizes {
        let voter = VoterKey::from_ed25519_seed(&seed(0));
        let mut keys: Vec<[u8; 32]> = (0..n)
            .map(|i| *VoterKey::from_ed25519_seed(&seed(i)).public_key())
            .collect();
        keys.sort_unstable();
        let t = Instant::now();
        let ring = Ring::new(&keys)?;
        let build = t.elapsed();
        let poll = PollKeys::new(b"measure-group-0123456789abcdef01", &[3u8; 16], &[4u8; 32])?;
        let choices = Choices::new(&[1])?;
        let t = Instant::now();
        let row = cast(&poll, &ring, &voter, &choices, None)?.row;
        let cast_time = t.elapsed();
        let t = Instant::now();
        open_and_verify(&poll, &ring, &row)?;
        let verify_time = t.elapsed();
        let proof = open_row(&poll, &row)?.proof;
        writeln!(
            out,
            "| {n} | {} | {} | {} | {:.1} | {:.1} | {:.1} |",
            proof.digits(),
            proof.encode().len(),
            row.len(),
            build.as_secs_f64() * 1e3,
            cast_time.as_secs_f64() * 1e3,
            verify_time.as_secs_f64() * 1e3,
        )?;
    }
    Ok(())
}
