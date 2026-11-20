// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Wall-clock timing of the credential operations, for the module specs.
//! Run with `cargo run --release -p lakko-anon-cred --example measure`.
//! Prints the median of each operation over a fixed number of runs.

use std::time::{Duration, Instant};

use lakko_anon_cred::{
    verify_registration, CredentialSecret, CredentialVerifier, IssuerSecretKey, MasterSecret,
    PollScope, PRESENTATION_LEN,
};

const RUNS: usize = 200;

fn median<T, F: FnMut() -> T>(mut f: F) -> Duration {
    let mut times: Vec<Duration> = (0..RUNS)
        .map(|_| {
            let start = Instant::now();
            std::hint::black_box(f());
            start.elapsed()
        })
        .collect();
    times.sort();
    times[RUNS / 2]
}

fn main() -> Result<(), lakko_anon_cred::AnonCredError> {
    let master = MasterSecret::from_bytes([9; 32]);
    let period = 700;
    let key = IssuerSecretKey::derive(&master, period)?;
    let public = key.public_key();
    let secret = CredentialSecret::from_lineage_seed(&[1; 32])?;
    let commitment = secret.commitment();
    let registration = secret.register()?;
    let request = secret.issuance_request(&public)?;
    let response = key.issue(&request, &commitment)?;
    let credential = secret.finalize_issuance(&public, &response)?;
    let scope = PollScope::channel_post([2; 16], 5);
    let presentation = secret
        .present(&credential, &public, &scope, b"labels")?
        .to_bytes();
    let decoded = lakko_anon_cred::Presentation::decode(&presentation)?;
    let verifier = CredentialVerifier::new(master.clone());

    let rows = [
        (
            "derive issuer key",
            median(|| IssuerSecretKey::derive(&master, period)),
        ),
        ("register (client)", median(|| secret.register())),
        (
            "verify registration (auth)",
            median(|| verify_registration(&registration)),
        ),
        (
            "issuance request (client)",
            median(|| secret.issuance_request(&public)),
        ),
        ("issue (auth)", median(|| key.issue(&request, &commitment))),
        (
            "finalize issuance (client)",
            median(|| secret.finalize_issuance(&public, &response)),
        ),
        (
            "present (client)",
            median(|| secret.present(&credential, &public, &scope, b"labels")),
        ),
        (
            "verify presentation (channels, incl. key derivation)",
            median(|| verifier.verify(&presentation, &scope, b"labels", period)),
        ),
        (
            "verify presentation (key already derived)",
            median(|| key.verify_presentation(&decoded, &scope, b"labels")),
        ),
    ];
    println!(
        "presentation size: {PRESENTATION_LEN} bytes (measured: {})",
        presentation.len()
    );
    for (name, time) in rows {
        println!("{name}: {:.1} us", time.as_secs_f64() * 1e6);
    }
    Ok(())
}
