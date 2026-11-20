// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `lakko-release-manifest` — the OFFLINE publisher tool for the ADR-0038
//! signed "update available" notice.
//!
//! Two subcommands:
//!
//! - `keygen` — generate a fresh Ed25519 release-signing keypair. The 32-byte
//!   PRIVATE seed is written to a file (default `release-signing-key.seed`, mode
//!   `0600`) the maintainer keeps OFFLINE; the base64url (no-pad) PUBLIC
//!   verifying key is printed to stdout for pasting into each client build
//!   config (`VITE_LAKKO_RELEASE_KEY` / Info.plist `LakkoReleaseKey` /
//!   `BuildConfig.LAKKO_RELEASE_KEY`, wired in S4/S5).
//!
//! - `mint` — read the seed, sign a [`ReleaseManifest`] over `latest` /
//!   `min_supported` / the current mint time, and write the raw
//!   [`RELEASE_MANIFEST_LEN`]-byte blob (binary, NOT JSON — it carries a 64-byte
//!   Ed25519 signature) to a file the maintainer copies to the static host.
//!
//! - `supporters` — sign the ADR-0097 supporters list from a plain text file
//!   (one name per line; blank lines skipped, each line trimmed) under the same
//!   release key and its own domain, and write the blob served at
//!   `/.well-known/lakko-supporters`.
//!
//! This is a maintainer CLI, not a service: it prints human-readable output to
//! stdout/stderr (English, no emoji) rather than logging via `tracing`, and it
//! MAY read the wall clock. The signing key is a dedicated publisher key —
//! `services/auth` is never in the loop (ADR-0038).

#![forbid(unsafe_code)]

use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_attest::{
    verify_release_manifest, verify_supporters_list, ReleaseManifest, SupportersList,
    RELEASE_MANIFEST_LEN,
};
use lakko_crypto::signing::SigningKey;

/// Default path for the private release-signing seed (32 bytes, mode `0600`).
const DEFAULT_SEED_FILE: &str = "release-signing-key.seed";

/// Default path for the minted raw manifest blob.
const DEFAULT_MANIFEST_OUT: &str = "release-manifest.bin";

/// The URL path clients pin (S4/S5 MUST pin the SAME path). Served on the
/// `lakko.me` vhost as a static file.
const PINNED_SERVE_PATH: &str = "/.well-known/lakko-release";

/// The on-disk target the nginx `alias` points at (a root SEPARATE from the web
/// SPA, so `just deploy-web`'s `rsync --delete` cannot wipe the manifest).
const SERVER_TARGET: &str = "/var/www/lakko-release/lakko-release.bin";

/// The URL path the clients fetch the supporters list from (ADR-0097).
const SUPPORTERS_SERVE_PATH: &str = "/.well-known/lakko-supporters";

/// The on-disk target of the supporters list (the same separate root as the
/// release manifest).
const SUPPORTERS_SERVER_TARGET: &str = "/var/www/lakko-release/lakko-supporters.bin";

/// Length of an Ed25519 seed / signing key in bytes.
const SEED_LEN: usize = 32;

const USAGE: &str = "\
lakko-release-manifest — offline publisher tool for the ADR-0038 update notice

usage:
  lakko-release-manifest keygen [--out <seedfile>]
  lakko-release-manifest mint --latest <N> --min-supported <N> [--key <seedfile>] [--out <path>]
  lakko-release-manifest supporters --version <N> --names <file> [--seed <seedfile>] [--out <path>]

keygen  generate the Ed25519 release-signing keypair (private seed -> file 0600,
        public key -> stdout as base64url for the client build configs)
mint    sign a release manifest and write the raw 89-byte blob to a file
supporters
        sign the supporters list (one name per line) and write the blob";

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
    let (sub, rest) = args
        .split_first()
        .ok_or_else(|| "missing subcommand".to_string())?;
    match sub.as_str() {
        "keygen" => cmd_keygen(rest),
        "mint" => cmd_mint(rest),
        "supporters" => cmd_supporters(rest),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(())
        },
        other => Err(format!("unknown subcommand `{other}`")),
    }
}

/// `keygen` — generate a fresh keypair, write the private seed, print the public
/// key + the paste-into-build-config instructions.
fn cmd_keygen(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    reject_unknown(&flags, &["out"])?;
    let out = flag(&flags, "out").unwrap_or(DEFAULT_SEED_FILE);

    let signing = SigningKey::generate();
    let seed = signing.to_seed();
    write_seed_0600(Path::new(out), &seed)?;
    let public_b64 = URL_SAFE_NO_PAD.encode(signing.verifying_key().to_bytes());

    println!("Release signing keypair generated.");
    println!();
    println!("  private seed written to : {out}  (mode 0600 — keep this OFFLINE, never commit)");
    println!("  public verifying key    : {public_b64}");
    println!();
    println!("Paste the public key into each client build config (wired in S4/S5):");
    println!("  web     -> VITE_LAKKO_RELEASE_KEY");
    println!("  iOS     -> Info.plist  LakkoReleaseKey");
    println!("  Android -> BuildConfig.LAKKO_RELEASE_KEY");
    println!();
    println!(
        "Anyone holding the seed file can mint release manifests. Store it offline; \
         never commit it to the repo."
    );
    Ok(())
}

/// `mint` — read the seed, sign the manifest over `latest`/`min_supported`/now,
/// write the raw blob, self-verify it, and print the publish reminder.
fn cmd_mint(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    reject_unknown(&flags, &["latest", "min-supported", "key", "out"])?;
    let latest = parse_u64(&flags, "latest")?;
    let min_supported = parse_u64(&flags, "min-supported")?;
    let key_path = flag(&flags, "key").unwrap_or(DEFAULT_SEED_FILE);
    let out = flag(&flags, "out").unwrap_or(DEFAULT_MANIFEST_OUT);

    if min_supported > latest {
        return Err(format!(
            "--min-supported ({min_supported}) cannot exceed --latest ({latest})"
        ));
    }

    let signing = read_signing_key(Path::new(key_path))?;
    let signed_at = now_unix_millis()?;
    let wire = ReleaseManifest::new(latest, min_supported, signed_at).sign(&signing);
    fs::write(out, wire).map_err(|e| format!("cannot write manifest `{out}`: {e}"))?;

    // Self-check: the freshly-minted blob must verify against our own public key
    // (freshness gate disabled — we are the mint, `signed_at == now`). A failure
    // here means a build-level bug, not operator error, so surface it loudly.
    verify_release_manifest(&wire, &signing.verifying_key(), signed_at, i64::MAX)
        .map_err(|e| format!("internal: freshly-minted manifest failed self-verification: {e}"))?;

    println!("Release manifest minted ({RELEASE_MANIFEST_LEN} bytes).");
    println!();
    println!("  latest_release        = {latest}");
    println!("  min_supported_release = {min_supported}");
    println!("  signed_at_unix_millis = {signed_at}");
    println!("  written to            = {out}");
    println!();
    println!("Publish (served immediately — no service restart, no nginx reload):");
    println!("  scp {out} lakkome@lakko.me:{SERVER_TARGET}");
    println!("  served at https://lakko.me{PINNED_SERVE_PATH}");
    println!();
    println!(
        "One-time on the VPS (if absent): \
         sudo install -d -o lakkome -g www-data -m 755 /var/www/lakko-release"
    );
    println!("See infra/README.md :: \"Release manifest (ADR-0038)\" for the full runbook.");
    Ok(())
}

/// `supporters` — read the names, sign the list under the release key, write
/// the blob, self-verify it, and print the publish reminder (ADR-0097).
fn cmd_supporters(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    reject_unknown(&flags, &["version", "names", "seed", "out"])?;
    let version = parse_u64(&flags, "version")?;
    let names_path = flag(&flags, "names").ok_or("missing required flag --names")?;
    let key_path = flag(&flags, "seed").unwrap_or(DEFAULT_SEED_FILE);
    let out = flag(&flags, "out").unwrap_or("lakko-supporters.bin");

    let text = fs::read_to_string(names_path)
        .map_err(|e| format!("cannot read names file `{names_path}`: {e}"))?;
    let names = names_from_text(&text);
    let signing = read_signing_key(Path::new(key_path))?;
    let list = SupportersList {
        list_version: version,
        signed_at_unix_millis: now_unix_millis()?,
        names,
    };
    let wire = list
        .sign(&signing)
        .map_err(|e| format!("cannot sign the supporters list: {e}"))?;
    fs::write(out, &wire).map_err(|e| format!("cannot write list `{out}`: {e}"))?;
    let got = verify_supporters_list(&wire, &signing.verifying_key())
        .map_err(|e| format!("internal: the signed list failed self-verification: {e}"))?;
    if got != list {
        return Err("internal: the signed list did not read back as written".into());
    }

    println!("Supporters list signed ({} bytes).", wire.len());
    println!();
    println!("  list_version = {version}");
    println!("  names        = {}", list.names.len());
    println!("  written to   = {out}");
    println!();
    println!("Publish:");
    println!("  scp {out} lakkome@lakko.me:{SUPPORTERS_SERVER_TARGET}");
    println!("  served at https://lakko.me{SUPPORTERS_SERVE_PATH}");
    Ok(())
}

/// One name per line: each line trimmed, blank lines skipped.
fn names_from_text(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Write the 32-byte seed atomically, refusing to overwrite an existing file
/// (a rotation must be deliberate — move the old seed aside first). On Unix the
/// file is created mode `0600` from the outset (no world-readable window).
fn write_seed_0600(path: &Path, seed: &[u8; SEED_LEN]) -> Result<(), String> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut file = opts.open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            format!(
                "seed file `{}` already exists — refusing to overwrite \
                 (move it aside to rotate the release key)",
                path.display()
            )
        } else {
            format!("cannot create seed file `{}`: {e}", path.display())
        }
    })?;
    file.write_all(seed)
        .map_err(|e| format!("cannot write seed file `{}`: {e}", path.display()))
}

/// Read a 32-byte seed file and reconstruct the signing key.
fn read_signing_key(path: &Path) -> Result<SigningKey, String> {
    let bytes =
        fs::read(path).map_err(|e| format!("cannot read seed file `{}`: {e}", path.display()))?;
    let seed: [u8; SEED_LEN] = bytes.as_slice().try_into().map_err(|_| {
        format!(
            "seed file `{}` must be exactly {SEED_LEN} bytes, got {}",
            path.display(),
            bytes.len()
        )
    })?;
    Ok(SigningKey::from_seed(&seed))
}

/// Current Unix time in milliseconds as an `i64` (matching the manifest's
/// `signed_at_unix_millis` field).
fn now_unix_millis() -> Result<i64, String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_string())?
        .as_millis();
    i64::try_from(millis).map_err(|_| "current time overflows i64 milliseconds".to_string())
}

/// Parse `--flag value` pairs into an ordered list. Rejects a bare token that is
/// not a `--flag`, and a `--flag` with no following value.
fn parse_flags(args: &[String]) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let token = &args[i];
        let key = token
            .strip_prefix("--")
            .ok_or_else(|| format!("expected a --flag, got `{token}`"))?;
        if key.is_empty() {
            return Err("empty flag name `--`".to_string());
        }
        let value = args
            .get(i + 1)
            .ok_or_else(|| format!("flag --{key} needs a value"))?;
        out.push((key.to_string(), value.clone()));
        i += 2;
    }
    Ok(out)
}

/// Look up the first value for `name` among the parsed flags.
fn flag<'a>(flags: &'a [(String, String)], name: &str) -> Option<&'a str> {
    flags
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// Parse a required `u64` flag.
fn parse_u64(flags: &[(String, String)], name: &str) -> Result<u64, String> {
    let value = flag(flags, name).ok_or_else(|| format!("missing required flag --{name}"))?;
    value
        .parse::<u64>()
        .map_err(|_| format!("--{name} must be a non-negative integer, got `{value}`"))
}

/// Reject any flag not in `allowed` (typo / wrong-subcommand guard).
fn reject_unknown(flags: &[(String, String)], allowed: &[&str]) -> Result<(), String> {
    for (key, _) in flags {
        if !allowed.contains(&key.as_str()) {
            return Err(format!("unknown flag --{key}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn unique_tmpdir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("lakko-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn mint_bytes_verify_roundtrip() {
        // In-memory: mint the wire blob and verify it against the derived public
        // key with the freshness gate disabled (signed_at == now).
        let signing = SigningKey::generate();
        let signed_at = now_unix_millis().unwrap();
        let wire = ReleaseManifest::new(7, 3, signed_at).sign(&signing);
        assert_eq!(wire.len(), RELEASE_MANIFEST_LEN);
        assert_eq!(wire.len(), 89);
        let got = verify_release_manifest(&wire, &signing.verifying_key(), signed_at, i64::MAX)
            .expect("a freshly-minted manifest verifies");
        assert_eq!(got.latest_release, 7);
        assert_eq!(got.min_supported_release, 3);
    }

    #[test]
    fn keygen_then_mint_files_roundtrip() {
        // Full file-based flow: keygen writes a 32-byte seed; mint reads it and
        // writes an 89-byte blob that verifies against the seed's public key.
        let dir = unique_tmpdir("rmt-roundtrip");
        let seed_path = dir.join("key.seed");
        let out_path = dir.join("manifest.bin");

        cmd_keygen(&["--out".into(), seed_path.display().to_string()]).unwrap();
        let seed_bytes = fs::read(&seed_path).unwrap();
        assert_eq!(seed_bytes.len(), SEED_LEN);
        let seed: [u8; SEED_LEN] = seed_bytes.as_slice().try_into().unwrap();
        let signing = SigningKey::from_seed(&seed);

        cmd_mint(&[
            "--latest".into(),
            "42".into(),
            "--min-supported".into(),
            "30".into(),
            "--key".into(),
            seed_path.display().to_string(),
            "--out".into(),
            out_path.display().to_string(),
        ])
        .unwrap();

        let wire = fs::read(&out_path).unwrap();
        assert_eq!(wire.len(), RELEASE_MANIFEST_LEN);
        let got = verify_release_manifest(
            &wire,
            &signing.verifying_key(),
            now_unix_millis().unwrap(),
            i64::MAX,
        )
        .expect("the minted file verifies against the seed's public key");
        assert_eq!(got.latest_release, 42);
        assert_eq!(got.min_supported_release, 30);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn mint_rejects_min_supported_above_latest() {
        let dir = unique_tmpdir("rmt-badrange");
        let seed_path = dir.join("key.seed");
        cmd_keygen(&["--out".into(), seed_path.display().to_string()]).unwrap();
        let err = cmd_mint(&[
            "--latest".into(),
            "5".into(),
            "--min-supported".into(),
            "9".into(),
            "--key".into(),
            seed_path.display().to_string(),
            "--out".into(),
            dir.join("m.bin").display().to_string(),
        ])
        .unwrap_err();
        assert!(err.contains("min-supported"), "unexpected error: {err}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn keygen_refuses_to_overwrite() {
        let dir = unique_tmpdir("rmt-nooverwrite");
        let seed_path = dir.join("key.seed");
        cmd_keygen(&["--out".into(), seed_path.display().to_string()]).unwrap();
        let err = cmd_keygen(&["--out".into(), seed_path.display().to_string()]).unwrap_err();
        assert!(err.contains("already exists"), "unexpected error: {err}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_flags_rejects_bare_token_and_missing_value() {
        assert!(parse_flags(&["latest".into()]).is_err());
        assert!(parse_flags(&["--latest".into()]).is_err());
        assert!(parse_flags(&["--".into(), "x".into()]).is_err());
        let ok = parse_flags(&["--latest".into(), "42".into()]).unwrap();
        assert_eq!(flag(&ok, "latest"), Some("42"));
    }

    #[test]
    fn supporters_file_round_trips_and_refuses_a_bad_name() {
        let dir = unique_tmpdir("rmt-supporters");
        let seed_path = dir.join("key.seed");
        let names = dir.join("names.txt");
        let out = dir.join("list.bin");
        cmd_keygen(&["--out".into(), seed_path.display().to_string()]).unwrap();
        fs::write(&names, "Ada\r\n\n  Grace Hopper  \n").unwrap();
        cmd_supporters(&[
            "--version".into(),
            "4".into(),
            "--names".into(),
            names.display().to_string(),
            "--seed".into(),
            seed_path.display().to_string(),
            "--out".into(),
            out.display().to_string(),
        ])
        .unwrap();
        let seed: [u8; SEED_LEN] = fs::read(&seed_path).unwrap().as_slice().try_into().unwrap();
        let got = verify_supporters_list(
            &fs::read(&out).unwrap(),
            &SigningKey::from_seed(&seed).verifying_key(),
        )
        .unwrap();
        assert_eq!(got.list_version, 4);
        assert_eq!(
            got.names,
            vec!["Ada".to_string(), "Grace Hopper".to_string()]
        );

        fs::write(&names, "ok\nbad\u{200B}name\n").unwrap();
        let err = cmd_supporters(&[
            "--version".into(),
            "5".into(),
            "--names".into(),
            names.display().to_string(),
            "--seed".into(),
            seed_path.display().to_string(),
            "--out".into(),
            out.display().to_string(),
        ])
        .unwrap_err();
        assert!(err.contains("name 1"), "unexpected error: {err}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reject_unknown_flags() {
        let flags = parse_flags(&["--bogus".into(), "1".into()]).unwrap();
        assert!(reject_unknown(&flags, &["latest"]).is_err());
    }
}
