// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `lakko-bootstrap-config` - the OFFLINE publisher tool for the ADR-0026
//! signed bootstrap config.
//!
//! - `keygen` generates the Ed25519 bootstrap-signing keypair. The 32-byte
//!   private seed goes to a file (default `bootstrap-signing-key.seed`, mode
//!   `0600`) kept offline; the public key is printed in standard base64, the
//!   form every client pins (`LakkoBootstrapTrustAnchor`,
//!   `LAKKO_BOOTSTRAP_TRUST_ANCHOR`, `LAKKO_DESKTOP_BOOTSTRAP_TRUST_ANCHOR`).
//! - `sign` reads a JSON array of transport specs, signs a config valid for
//!   `--ttl-days`, verifies what it wrote, and writes the JSON file the clients
//!   fetch.
//!
//! A maintainer CLI, not a service: it prints to stdout and stderr rather than
//! logging through `tracing`, and it may read the wall clock.

#![forbid(unsafe_code)]

use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use lakko_crypto::signing::SigningKey;
use lakko_transport::{BootstrapConfig, TransportSpec, SCHEMA_VERSION};

const DEFAULT_SEED_FILE: &str = "bootstrap-signing-key.seed";
const DEFAULT_OUT: &str = "bootstrap.json";
const SECS_PER_DAY: u64 = 86_400;

const USAGE: &str = "\
lakko-bootstrap-config - offline publisher tool for the ADR-0026 bootstrap config

usage:
  lakko-bootstrap-config keygen [--out <seedfile>]
  lakko-bootstrap-config sign --transports <file.json> --ttl-days <N> [--key <seedfile>] [--out <path>]

keygen  generate the signing keypair (private seed -> file 0600, public key ->
        stdout as base64 for the client build configs)
sign    sign a config from a JSON array of transport specs and write the JSON
        file clients fetch";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!();
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        },
    }
}

fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("keygen") => {
            keygen(&option(args, "--out")?.unwrap_or_else(|| DEFAULT_SEED_FILE.into()))
        },
        Some("sign") => {
            let transports = option(args, "--transports")?.ok_or("--transports is required")?;
            let days = option(args, "--ttl-days")?.ok_or("--ttl-days is required")?;
            let days: u64 = days.parse().map_err(|_| "--ttl-days is not a number")?;
            let key = option(args, "--key")?.unwrap_or_else(|| DEFAULT_SEED_FILE.into());
            let out = option(args, "--out")?.unwrap_or_else(|| DEFAULT_OUT.into());
            sign_file(&transports, days, &key, &out)
        },
        _ => Err("expected keygen or sign".to_owned()),
    }
}

/// The value after `name`, if it is given.
fn option(args: &[String], name: &str) -> Result<Option<String>, String> {
    match args.iter().position(|a| a == name) {
        None => Ok(None),
        Some(i) => args
            .get(i + 1)
            .cloned()
            .map(Some)
            .ok_or_else(|| format!("{name} needs a value")),
    }
}

fn keygen(path: &str) -> Result<(), String> {
    if Path::new(path).exists() {
        return Err(format!(
            "{path} already exists; not overwriting a signing key"
        ));
    }
    let key = SigningKey::generate();
    write_private(path, &key.to_seed())?;
    println!("private seed: {path} (mode 0600, keep offline, never commit)");
    println!(
        "public key:   {}",
        STANDARD.encode(key.verifying_key().to_bytes())
    );
    Ok(())
}

#[cfg(unix)]
fn write_private(path: &str, bytes: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("{path}: {e}"))?;
    file.write_all(bytes).map_err(|e| format!("{path}: {e}"))
}

#[cfg(not(unix))]
fn write_private(path: &str, bytes: &[u8]) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("{path}: {e}"))?;
    file.write_all(bytes).map_err(|e| format!("{path}: {e}"))
}

fn read_seed(path: &str) -> Result<SigningKey, String> {
    let bytes = fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let seed: [u8; 32] = bytes
        .try_into()
        .map_err(|_| format!("{path} is not a 32-byte seed"))?;
    Ok(SigningKey::from_seed(&seed))
}

/// Sign `transports`, valid from `now` for `ttl_days`, and check the result the
/// way a client will.
fn signed(
    transports: Vec<TransportSpec>,
    ttl_days: u64,
    key: &SigningKey,
    now: u64,
) -> Result<BootstrapConfig, String> {
    if transports.is_empty() {
        return Err("a config needs at least one transport".to_owned());
    }
    if ttl_days == 0 {
        return Err("--ttl-days must be at least 1".to_owned());
    }
    let mut config = BootstrapConfig {
        version: SCHEMA_VERSION,
        issued_at: now,
        expires_at: now + ttl_days.saturating_mul(SECS_PER_DAY),
        transports,
        signature_hex: String::new(),
    };
    config.sign(key).map_err(|e| format!("sign: {e}"))?;
    let at = UNIX_EPOCH + std::time::Duration::from_secs(now);
    config
        .verify(&key.verifying_key(), at)
        .map_err(|e| format!("the signed config does not verify: {e}"))?;
    Ok(config)
}

fn sign_file(transports: &str, ttl_days: u64, key: &str, out: &str) -> Result<(), String> {
    let raw = fs::read(transports).map_err(|e| format!("{transports}: {e}"))?;
    let specs: Vec<TransportSpec> =
        serde_json::from_slice(&raw).map_err(|e| format!("{transports}: {e}"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let config = signed(specs, ttl_days, &read_seed(key)?, now)?;
    let json = serde_json::to_vec_pretty(&config).map_err(|e| e.to_string())?;
    fs::write(out, json).map_err(|e| format!("{out}: {e}"))?;
    println!("wrote {out}, expires at unix {}", config.expires_at);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_transport::ShadowsocksConfig;

    fn spec() -> TransportSpec {
        TransportSpec::Shadowsocks2022(ShadowsocksConfig {
            remote: "203.0.113.7:8388".to_owned(),
            method: "2022-blake3-chacha20-poly1305".to_owned(),
            psk_b64: STANDARD.encode([9u8; 32]),
        })
    }

    #[test]
    fn a_signed_config_verifies_under_the_public_key_only() {
        let key = SigningKey::generate();
        let config = signed(vec![spec()], 7, &key, 1_000).unwrap();
        assert_eq!(config.expires_at, 1_000 + 7 * SECS_PER_DAY);
        let at = UNIX_EPOCH + std::time::Duration::from_secs(1_000);
        assert!(config.verify(&key.verifying_key(), at).is_ok());
        let other = SigningKey::generate().verifying_key();
        assert!(config.verify(&other, at).is_err());
    }

    #[test]
    fn an_empty_config_or_lifetime_is_refused() {
        let key = SigningKey::generate();
        assert!(signed(vec![], 7, &key, 1_000).is_err());
        assert!(signed(vec![spec()], 0, &key, 1_000).is_err());
    }
}
