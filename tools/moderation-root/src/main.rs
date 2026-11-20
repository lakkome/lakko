// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `lakko-moderation-root` — the OFFLINE tool for the ADR-0099 moderator
//! list (section 3).
//!
//! - `keygen` — generate the Ed25519 root key. The 32-byte private seed goes
//!   to a file (mode `0600`) kept offline; the base64url public key is
//!   printed for the pins (`LAKKO_MODERATION_ROOT_KEY` on the moderation
//!   service, `VITE_LAKKO_MODERATION_ROOT_KEY`, Info.plist
//!   `LakkoModerationRootKey`, `BuildConfig.LAKKO_MODERATION_ROOT_KEY`).
//! - `sign-list` — read the seed and a JSON file of moderators, sign the
//!   list at `--version`, write the raw signed bytes and check them back.
//! - `dev-moderator` — for a test stack only: a moderator whose report key
//!   comes from a random 32-byte file instead of a security key's PRF
//!   output, and whose credential is a software Ed25519 "security key" whose
//!   seed is written to a second file (mode `0600`). It prints the JSON
//!   entry to put in the list. Never for a real deployment: both secrets are
//!   on disk.
//! - `dev-sign` — for a test stack only: sign any moderator action with
//!   that software key and print the JSON body of the moderation service's
//!   `POST /internal/statements`. The target is `--account-key` (an
//!   account's identity key, hashed to its target) for an account action, or
//!   `--target` for any action: the raw target bytes of the action's kind
//!   (account target 32, group handle lookup key 64, channel id 16, username
//!   lookup key 64, blob id 32), in hex or base64. The list file and its
//!   root key say which moderator the key is and which list it signs under;
//!   the signed statement is verified against that list before it is
//!   printed.
//!
//! Moderators file (`--entries`): a JSON array of
//! `{"moderator_id": <32 hex>, "name": "...", "key_type": "ed25519" |
//! "p256", "key": <base64url raw key>, "credential_id": <base64url>,
//! "x25519": <base64url 32 bytes>, "added_at_ms": N, "revoked_at_ms": N}`.
//!
//! A maintainer CLI: it prints to stdout / stderr and reads the clock.

#![forbid(unsafe_code)]

use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use lakko_crypto::signing::SigningKey;
use lakko_moderation::moderators::{ModeratorEntry, ModeratorKey, ModeratorList};
use lakko_moderation::seal::moderator_report_secret;
use lakko_moderation::software_authenticator::SoftwareAuthenticator;
use lakko_moderation::statement::{reason_commitment, Action, Statement};
use lakko_moderation::target::{Target, TargetKind};
use lakko_moderation::Reason;
use rand::RngCore as _;
use serde::{Deserialize, Serialize};

const USAGE: &str = "\
lakko-moderation-root — offline tool for the ADR-0099 moderator list

usage:
  lakko-moderation-root keygen [--out <seedfile>]
  lakko-moderation-root sign-list --version <N> --entries <json> --rp-id <host>
                                  --origin <https://host> [--key <seedfile>] [--out <path>]
  lakko-moderation-root dev-moderator --name <name> [--out <secretfile>]
                                      [--signing-key-out <seedfile>]
  lakko-moderation-root dev-sign --signing-key <seedfile> --list <moderators.bin>
                                 --root <base64url> --action <action>
                                 (--account-key <base64> | --target <hex|base64>)
                                 --reason <name|byte> [--until-ms <N>]

actions: suspend, lift, take-back-name, release-name, hide-from-search,
  show-in-search, channel-delist, channel-block-handle, channel-remove,
  channel-restore, group-delist, group-block-handle, group-restore,
  blob-delete, keep-suspension

A signed statement is applied only within a day of signing (and no more
than five minutes ahead of the applying service's clock): post it soon.";

const DEFAULT_SEED_FILE: &str = "moderation-root.seed";
const DEFAULT_LIST_OUT: &str = "moderators.bin";
const DEFAULT_DEV_SECRET: &str = "dev-moderator.secret";
const DEFAULT_DEV_SIGNING_KEY: &str = "dev-moderator.signing.seed";

#[derive(Debug, Serialize, Deserialize)]
struct EntryJson {
    moderator_id: String,
    name: String,
    key_type: String,
    key: String,
    credential_id: String,
    x25519: String,
    #[serde(default)]
    added_at_ms: u64,
    #[serde(default)]
    revoked_at_ms: u64,
}

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
    let flags = parse_flags(rest)?;
    match sub.as_str() {
        "keygen" => keygen(&flags),
        "sign-list" => sign_list(&flags),
        "dev-moderator" => dev_moderator(&flags),
        "dev-sign" => dev_sign(&flags),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(())
        },
        other => Err(format!("unknown subcommand `{other}`")),
    }
}

fn keygen(flags: &[(String, String)]) -> Result<(), String> {
    reject_unknown(flags, &["out"])?;
    let out = flag(flags, "out").unwrap_or(DEFAULT_SEED_FILE);
    let signing = SigningKey::generate();
    write_secret_0600(Path::new(out), &signing.to_seed())?;
    println!("Moderation root key generated.");
    println!();
    println!("  private seed written to : {out}  (mode 0600, keep it OFFLINE, never commit)");
    println!(
        "  public key              : {}",
        URL_SAFE_NO_PAD.encode(signing.verifying_key().to_bytes())
    );
    println!();
    println!("Pin the public key in:");
    println!("  moderation service -> LAKKO_MODERATION_ROOT_KEY");
    println!("  web                -> VITE_LAKKO_MODERATION_ROOT_KEY");
    println!("  iOS                -> Info.plist LakkoModerationRootKey");
    println!("  Android            -> BuildConfig.LAKKO_MODERATION_ROOT_KEY");
    Ok(())
}

fn sign_list(flags: &[(String, String)]) -> Result<(), String> {
    reject_unknown(
        flags,
        &["version", "entries", "rp-id", "origin", "key", "out"],
    )?;
    let version = flag(flags, "version")
        .ok_or("missing --version")?
        .parse::<u64>()
        .map_err(|_| "--version must be a positive integer".to_string())?;
    let entries_path = flag(flags, "entries").ok_or("missing --entries")?;
    let rp_id = flag(flags, "rp-id").ok_or("missing --rp-id")?;
    let origin = flag(flags, "origin").ok_or("missing --origin")?;
    let key_path = flag(flags, "key").unwrap_or(DEFAULT_SEED_FILE);
    let out = flag(flags, "out").unwrap_or(DEFAULT_LIST_OUT);

    let json = fs::read_to_string(entries_path)
        .map_err(|e| format!("cannot read `{entries_path}`: {e}"))?;
    let raw: Vec<EntryJson> =
        serde_json::from_str(&json).map_err(|e| format!("`{entries_path}`: {e}"))?;
    let entries = raw
        .into_iter()
        .map(entry_from_json)
        .collect::<Result<Vec<_>, _>>()?;
    let seed = read_secret(Path::new(key_path))?;
    let list = ModeratorList {
        list_version: version,
        issued_at_ms: now_ms()?,
        rp_id: rp_id.to_owned(),
        origin: origin.to_owned(),
        entries,
    };
    let bytes = list
        .sign(&seed)
        .map_err(|e| format!("the list is refused: {e}"))?;
    let root = SigningKey::from_seed(&seed).verifying_key().to_bytes();
    let back = ModeratorList::verify(&bytes, &root)
        .map_err(|e| format!("internal: the signed list does not verify: {e}"))?;
    fs::write(out, &bytes).map_err(|e| format!("cannot write `{out}`: {e}"))?;
    println!(
        "Moderator list version {version} signed ({} bytes).",
        bytes.len()
    );
    println!("  moderators : {}", back.entries.len());
    println!("  written to : {out}");
    println!();
    println!("Install it at LAKKO_MODERATION_LIST_PATH on the moderation service and restart it.");
    println!("A service refuses a list it cannot verify; clients refuse a lower version.");
    Ok(())
}

fn dev_moderator(flags: &[(String, String)]) -> Result<(), String> {
    reject_unknown(flags, &["name", "out", "signing-key-out"])?;
    let name = flag(flags, "name").ok_or("missing --name")?;
    let out = flag(flags, "out").unwrap_or(DEFAULT_DEV_SECRET);
    let signing_out = flag(flags, "signing-key-out").unwrap_or(DEFAULT_DEV_SIGNING_KEY);
    let mut prf = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut prf);
    let secret = moderator_report_secret(&prf).map_err(|e| e.to_string())?;
    let mut signing_seed = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut signing_seed);
    write_secret_0600(Path::new(out), &prf)?;
    write_secret_0600(Path::new(signing_out), &signing_seed)?;
    let mut id = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut id);
    let mut credential = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut credential);
    let entry = EntryJson {
        moderator_id: id.iter().fold(String::new(), |mut acc, b| {
            use std::fmt::Write as _;
            let _ = write!(acc, "{b:02x}");
            acc
        }),
        name: name.to_owned(),
        key_type: "ed25519".into(),
        key: URL_SAFE_NO_PAD.encode(
            SigningKey::from_seed(&signing_seed)
                .verifying_key()
                .to_bytes(),
        ),
        credential_id: URL_SAFE_NO_PAD.encode(credential),
        x25519: URL_SAFE_NO_PAD.encode(secret.public_key().to_bytes()),
        added_at_ms: now_ms()?,
        revoked_at_ms: 0,
    };
    eprintln!("TEST STACK ONLY: the report key of this moderator is the file {out},");
    eprintln!("and its software security key is the file {signing_out} (for dev-sign).");
    println!(
        "{}",
        serde_json::to_string_pretty(&entry).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn dev_sign(flags: &[(String, String)]) -> Result<(), String> {
    reject_unknown(
        flags,
        &[
            "signing-key",
            "list",
            "root",
            "action",
            "account-key",
            "target",
            "reason",
            "until-ms",
        ],
    )?;
    let seed = read_secret(Path::new(
        flag(flags, "signing-key").ok_or("missing --signing-key")?,
    ))?;
    let list_path = flag(flags, "list").ok_or("missing --list")?;
    let root: [u8; 32] = b64(flag(flags, "root").ok_or("missing --root")?)?
        .try_into()
        .map_err(|_| "--root is a 32-byte key".to_string())?;
    let action_name = flag(flags, "action").ok_or("missing --action")?;
    let action = Action::from_name(action_name)
        .map_err(|_| format!("unknown --action `{action_name}` (see the usage)"))?;
    let target = parse_target(action, flag(flags, "account-key"), flag(flags, "target"))?;
    let reason = parse_reason(flag(flags, "reason").ok_or("missing --reason")?)?;
    let until_ms = match flag(flags, "until-ms") {
        None => 0,
        Some(_) if action != Action::Suspend => return Err("--until-ms is for a suspension".into()),
        Some(v) => v
            .parse::<u64>()
            .map_err(|_| "--until-ms must be a positive integer".to_string())?,
    };

    let bytes = fs::read(list_path).map_err(|e| format!("cannot read `{list_path}`: {e}"))?;
    let list = ModeratorList::verify(&bytes, &root)
        .map_err(|e| format!("`{list_path}` does not verify under --root: {e}"))?;
    let authenticator = SoftwareAuthenticator::from_seed(&seed);
    let key = authenticator.moderator_key();
    let entry = list
        .entries
        .iter()
        .find(|e| e.key == key && e.revoked_at_ms == 0)
        .ok_or("this signing key is not a current moderator of the list")?;

    let mut statement_id = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut statement_id);
    let mut salt = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    let statement = Statement {
        statement_id,
        action,
        target,
        reason_commitment: reason_commitment(reason, &salt),
        until_ms,
        issued_at_ms: now_ms()?,
        moderator_id: entry.moderator_id,
        list_version: list.list_version,
    };
    let signed = authenticator
        .sign_statement(&list, &statement)
        .map_err(|e| format!("the statement is refused: {e}"))?;
    signed
        .verify(&list)
        .map_err(|e| format!("internal: the signed statement does not verify: {e}"))?;
    let encoded = signed.encode().map_err(|e| e.to_string())?;
    let body = serde_json::json!({
        "signed_statement_b64": STANDARD.encode(encoded),
        "reason": reason.as_u8(),
        "reason_salt_b64": STANDARD.encode(salt),
    });
    eprintln!("TEST STACK ONLY: signed with a software key. POST this to /internal/statements.");
    println!("{body}");
    Ok(())
}

/// The target of `action`: from `--account-key` (hashed) for an account
/// action, or from `--target`, raw bytes in hex or base64 of the kind's
/// length. Exactly one of the two.
fn parse_target(
    action: Action,
    account_key: Option<&str>,
    target: Option<&str>,
) -> Result<Target, String> {
    let kind = action.target_kind();
    match (account_key, target) {
        (Some(key), None) => {
            if kind != TargetKind::Account {
                return Err(format!(
                    "--account-key is for an account action; `{}` takes --target",
                    action.name()
                ));
            }
            let key: [u8; 32] = any_b64(key)?
                .try_into()
                .map_err(|_| "--account-key is a 32-byte key".to_string())?;
            Target::account(&key).map_err(|e| e.to_string())
        },
        (None, Some(raw)) => {
            let len = kind.target_len();
            let bytes = hex_exact(raw, len).map_or_else(|| any_b64(raw), Ok)?;
            Target::new(kind, &bytes).map_err(|_| {
                format!(
                    "--target for `{}` is {len} bytes, hex or base64",
                    action.name()
                )
            })
        },
        (Some(_), Some(_)) => Err("give --account-key or --target, not both".into()),
        (None, None) => Err("missing --target (or --account-key for an account)".into()),
    }
}

/// `text` as exactly `len` bytes of hex (either case), or `None`.
fn hex_exact(text: &str, len: usize) -> Option<Vec<u8>> {
    let t = text.trim();
    if t.len() != len * 2 || !t.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..len)
        .map(|i| u8::from_str_radix(t.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect()
}

fn parse_reason(s: &str) -> Result<Reason, String> {
    if let Ok(byte) = s.parse::<u8>() {
        return Reason::from_u8(byte).map_err(|_| format!("unknown reason `{s}`"));
    }
    Reason::ALL
        .into_iter()
        .find(|r| r.name() == s)
        .ok_or_else(|| format!("unknown reason `{s}`"))
}

/// Base64 in either alphabet, padded or not.
fn any_b64(s: &str) -> Result<Vec<u8>, String> {
    use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE};
    let t = s.trim();
    STANDARD
        .decode(t)
        .or_else(|_| STANDARD_NO_PAD.decode(t))
        .or_else(|_| URL_SAFE.decode(t))
        .or_else(|_| URL_SAFE_NO_PAD.decode(t))
        .map_err(|_| format!("`{s}` is not base64"))
}

fn entry_from_json(e: EntryJson) -> Result<ModeratorEntry, String> {
    let id = hex16(&e.moderator_id)?;
    let key_bytes = b64(&e.key)?;
    let key = match e.key_type.as_str() {
        "ed25519" => ModeratorKey::Ed25519(
            key_bytes
                .try_into()
                .map_err(|_| "an ed25519 key is 32 bytes".to_string())?,
        ),
        "p256" => ModeratorKey::P256(
            key_bytes
                .try_into()
                .map_err(|_| "a p256 key is 65 bytes, uncompressed".to_string())?,
        ),
        other => return Err(format!("unknown key_type `{other}`")),
    };
    Ok(ModeratorEntry {
        moderator_id: id,
        name: e.name,
        key,
        credential_id: b64(&e.credential_id)?,
        x25519_public: b64(&e.x25519)?
            .try_into()
            .map_err(|_| "an x25519 key is 32 bytes".to_string())?,
        added_at_ms: e.added_at_ms,
        revoked_at_ms: e.revoked_at_ms,
    })
}

fn hex16(s: &str) -> Result<[u8; 16], String> {
    if s.len() != 32 || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("moderator_id `{s}` is not 32 hex characters"));
    }
    let mut out = [0u8; 16];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

fn b64(s: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| format!("`{s}` is not base64url"))
}

fn now_ms() -> Result<u64, String> {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_string())?
        .as_millis();
    u64::try_from(ms).map_err(|_| "time overflows".to_string())
}

fn write_secret_0600(path: &Path, secret: &[u8; 32]) -> Result<(), String> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut file = opts.open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            format!("`{}` already exists, refusing to overwrite", path.display())
        } else {
            format!("cannot create `{}`: {e}", path.display())
        }
    })?;
    file.write_all(secret)
        .map_err(|e| format!("cannot write `{}`: {e}", path.display()))
}

fn read_secret(path: &Path) -> Result<[u8; 32], String> {
    let bytes = fs::read(path).map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| format!("`{}` must be exactly 32 bytes", path.display()))
}

fn parse_flags(args: &[String]) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let key = args[i]
            .strip_prefix("--")
            .filter(|k| !k.is_empty())
            .ok_or_else(|| format!("expected a --flag, got `{}`", args[i]))?;
        let value = args
            .get(i + 1)
            .ok_or_else(|| format!("flag --{key} needs a value"))?;
        out.push((key.to_string(), value.clone()));
        i += 2;
    }
    Ok(out)
}

fn flag<'a>(flags: &'a [(String, String)], name: &str) -> Option<&'a str> {
    flags
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

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

    #[test]
    fn every_action_takes_its_target() {
        let key = STANDARD.encode([7u8; 32]);
        for b in 1..=15u8 {
            let action = Action::from_u8(b).unwrap();
            let len = action.target_kind().target_len();
            let hex = "ab".repeat(len);
            let t = parse_target(action, None, Some(&hex)).unwrap();
            assert_eq!(t.bytes(), vec![0xab; len].as_slice());
            let b64 = STANDARD.encode(vec![0xcd; len]);
            assert_eq!(
                parse_target(action, None, Some(&b64)).unwrap().bytes(),
                vec![0xcd; len].as_slice()
            );
            let wrong = "ab".repeat(len + 1);
            assert!(parse_target(action, None, Some(&wrong)).is_err());
            let by_key = parse_target(action, Some(&key), None);
            if action.target_kind() == TargetKind::Account {
                assert_eq!(by_key.unwrap(), Target::account(&[7; 32]).unwrap());
            } else {
                assert!(by_key.is_err());
            }
            assert!(parse_target(action, Some(&key), Some(&hex)).is_err());
            assert!(parse_target(action, None, None).is_err());
        }
    }

    #[test]
    fn keep_suspension_is_signed_for_an_account() {
        let action = Action::from_name("keep-suspension").unwrap();
        assert_eq!(action, Action::KeepSuspension);
        let key = STANDARD.encode([7u8; 32]);
        assert_eq!(
            parse_target(action, Some(&key), None).unwrap(),
            Target::account(&[7; 32]).unwrap()
        );
    }

    #[test]
    fn hex_is_exact() {
        assert_eq!(hex_exact("0aFF", 2), Some(vec![0x0a, 0xff]));
        assert_eq!(hex_exact("0aF", 2), None);
        assert_eq!(hex_exact("0g00", 2), None);
    }
}
