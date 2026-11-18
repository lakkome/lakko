// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `lakko-donations` - the OFFLINE tool that signs the donation methods
//! discovery serves (`docs/ux/donations.md` section 3).
//!
//! - `keygen [--out <seedfile>]` - a fresh Ed25519 donations key. The 32-byte
//!   PRIVATE seed goes to a file created mode `0600` (default
//!   `donations-signing-key.seed`), kept OFFLINE; the base64url PUBLIC key is
//!   printed for the web build and for discovery's optional startup check.
//! - `sign [--key <seedfile>] --in <methods.json> [--out <signed.json>]` - read
//!   the plain methods file (the format discovery read before signing:
//!   `id`, `kind`, `link`, `qr_file`, `phone`, `coin`, `network`, `address`,
//!   `countries`, and on a crypto method `icon`: `btc`, `eth` or `ton`, the
//!   coin mark the client draws in the centre of the address's QR), sign one method statement per method and one phone
//!   statement per phone, embed each QR image, and write the signed file
//!   discovery loads as one systemd credential:
//!
//!   ```json
//!   {"methods": [{"id": "sbp", "countries": "RU,BY",
//!                 "statement_b64": "...", "phone_b64": "...", "qr_png_b64": "..."}]}
//!   ```
//!
//!   Standard base64 with padding. `countries` stays unsigned: which country
//!   sees which method is the server's choice. The output is verified
//!   against the key's own public half before it is written.
//!
//! A maintainer CLI, not a service: it prints to stdout / stderr (English, no
//! emoji) and reads the wall clock for `signed_at`.

#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use lakko_attest::{
    sign_donation_method, sign_donation_phone, verify_donation_method, verify_donation_phone,
    DonationIcon, DonationKind, DonationMethod,
};
use lakko_crypto::hash::sha256;
use lakko_crypto::signing::SigningKey;
use serde::{Deserialize, Serialize};

/// Default path of the private donations seed.
const DEFAULT_SEED_FILE: &str = "donations-signing-key.seed";
/// Default path of the signed output.
const DEFAULT_OUT: &str = "donations.signed.json";
/// Where the signed file goes on a server (the `LoadCredential=` source).
const SERVER_TARGET: &str = "/etc/lakko/credstore/lakko-donations.json";
/// Length of an Ed25519 seed.
const SEED_LEN: usize = 32;
/// Most methods the file may list (discovery refuses more).
const MAX_METHODS: usize = 16;
/// Largest QR image, in bytes (discovery refuses larger).
const MAX_QR_BYTES: usize = 512 * 1024;
/// Largest signed file, in bytes (discovery refuses larger).
const MAX_SIGNED_FILE_BYTES: usize = 1024 * 1024;

const USAGE: &str = "\
lakko-donations - offline tool for the signed donation methods

usage:
  lakko-donations keygen [--out <seedfile>]
  lakko-donations sign --in <methods.json> [--key <seedfile>] [--out <signed.json>]

keygen  generate the Ed25519 donations key (private seed -> file 0600,
        public key -> stdout as base64url)
sign    sign every method and phone of the plain methods file, embed the QR
        images, and write the signed file discovery loads";

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
        "sign" => cmd_sign(rest),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(())
        },
        other => Err(format!("unknown subcommand `{other}`")),
    }
}

/// One method of the plain input file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputMethod {
    id: String,
    kind: String,
    #[serde(default)]
    link: Option<String>,
    #[serde(default)]
    qr_file: Option<String>,
    #[serde(default)]
    phone: Option<String>,
    #[serde(default)]
    countries: Option<String>,
    #[serde(default)]
    coin: Option<String>,
    #[serde(default)]
    network: Option<String>,
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    icon: Option<String>,
}

/// The plain input file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputFile {
    methods: Vec<InputMethod>,
}

/// One method of the signed output file.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SignedMethod {
    /// The method id (also inside the statement).
    id: String,
    /// The unsigned country rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    countries: Option<String>,
    /// The signed method statement, standard base64.
    pub statement_b64: String,
    /// The signed phone statement, standard base64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone_b64: Option<String>,
    /// The QR PNG, standard base64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qr_png_b64: Option<String>,
}

/// The signed output file.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SignedFile {
    /// The methods in input order.
    methods: Vec<SignedMethod>,
}

/// `keygen`.
fn cmd_keygen(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    reject_unknown(&flags, &["out"])?;
    let out = flag(&flags, "out").unwrap_or(DEFAULT_SEED_FILE);
    let signing = SigningKey::generate();
    write_seed_0600(Path::new(out), &signing.to_seed())?;
    let public_b64 = URL_SAFE_NO_PAD.encode(signing.verifying_key().to_bytes());
    println!("Donations signing keypair generated.");
    println!();
    println!("  private seed written to : {out}  (mode 0600, keep it OFFLINE, never commit)");
    println!("  public verifying key    : {public_b64}");
    println!();
    println!("Pin the public key in:");
    println!("  the web build                -> VITE_LAKKO_DONATIONS_KEY");
    println!("  discovery (optional check)   -> LAKKO_DISCOVERY_DONATIONS_KEY");
    Ok(())
}

/// `sign`.
fn cmd_sign(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    reject_unknown(&flags, &["in", "key", "out"])?;
    let input = flag(&flags, "in").ok_or_else(|| "missing required flag --in".to_string())?;
    let key_path = flag(&flags, "key").unwrap_or(DEFAULT_SEED_FILE);
    let out = flag(&flags, "out").unwrap_or(DEFAULT_OUT);

    let signing = read_signing_key(Path::new(key_path))?;
    let json = fs::read_to_string(input).map_err(|e| format!("cannot read `{input}`: {e}"))?;
    let base = Path::new(input)
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let signed = sign_file(&json, &base, &signing, now_unix_millis()?)?;
    let text = serde_json::to_string_pretty(&signed)
        .map_err(|e| format!("internal: cannot serialise: {e}"))?;
    if text.len() > MAX_SIGNED_FILE_BYTES {
        return Err(format!(
            "the signed file is {} bytes, more than the {MAX_SIGNED_FILE_BYTES} bytes \
             discovery accepts; make the QR images smaller or list fewer methods",
            text.len()
        ));
    }
    write_private(Path::new(out), text.as_bytes())?;
    println!(
        "Signed {} donation method(s) into {out}.",
        signed.methods.len()
    );
    println!(
        "  public key: {}",
        URL_SAFE_NO_PAD.encode(signing.verifying_key().to_bytes())
    );
    println!();
    println!("Install on the server as root, then restart discovery:");
    println!("  install -m 600 -o root -g root {out} {SERVER_TARGET}");
    println!("See infra/README.md, \"Donation methods\".");
    Ok(())
}

/// Trim a text field; empty is absent.
fn trimmed(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Check the syntax discovery accepts for `countries`: two-letter codes, all
/// allowed or all refused with `!`.
fn check_countries(raw: &str) -> Result<(), String> {
    let tokens: Vec<&str> = raw
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    let refused = tokens.iter().filter(|t| t.starts_with('!')).count();
    if refused != 0 && refused != tokens.len() {
        return Err("countries mixes allowed and refused codes".to_string());
    }
    for t in tokens {
        let code = t.trim_start_matches('!');
        if code.len() != 2 || !code.bytes().all(|b| b.is_ascii_alphabetic()) {
            return Err(format!("countries holds `{t}`, not a two-letter code"));
        }
    }
    Ok(())
}

/// Sign the plain methods file. `base` resolves a relative `qr_file`;
/// `signed_at` is the stamp every statement carries. The result is verified
/// against the key's public half before it is returned.
fn sign_file(
    json: &str,
    base: &Path,
    signing: &SigningKey,
    signed_at: i64,
) -> Result<SignedFile, String> {
    let file: InputFile =
        serde_json::from_str(json).map_err(|e| format!("not a methods file: {e}"))?;
    if file.methods.len() > MAX_METHODS {
        return Err(format!("more than {MAX_METHODS} methods"));
    }
    let mut seen = HashSet::new();
    let mut methods = Vec::with_capacity(file.methods.len());
    for (index, m) in file.methods.into_iter().enumerate() {
        let fail = |e: String| format!("method {index} (`{}`): {e}", m.id);
        if !seen.insert(m.id.clone()) {
            return Err(fail("id appears twice".to_string()));
        }
        let kind = DonationKind::from_name(m.kind.trim())
            .ok_or_else(|| fail(format!("unknown kind `{}`", m.kind)))?;
        let countries = trimmed(m.countries.clone());
        if let Some(c) = &countries {
            check_countries(c).map_err(fail)?;
        }
        let qr_png = match trimmed(m.qr_file.clone()) {
            Some(path) => Some(read_qr(&base.join(path)).map_err(fail)?),
            None => None,
        };
        let icon = match trimmed(m.icon.clone()) {
            None => DonationIcon::None,
            Some(name) => DonationIcon::from_name(&name).ok_or_else(|| {
                fail(format!(
                    "unknown icon `{name}` (btc, eth or ton, or leave it out)"
                ))
            })?,
        };
        if icon != DonationIcon::None && kind != DonationKind::Crypto {
            return Err(fail("only a crypto method carries an icon".to_string()));
        }
        let phone = trimmed(m.phone.clone());
        let method = DonationMethod {
            signed_at_unix_millis: signed_at,
            id: m.id.clone(),
            kind,
            icon,
            link: trimmed(m.link.clone()),
            coin: trimmed(m.coin.clone()),
            network: trimmed(m.network.clone()),
            address: trimmed(m.address.clone()),
            qr_sha256: qr_png.as_deref().map(sha256),
            has_phone: phone.is_some(),
        };
        let statement = sign_donation_method(&method, signing).map_err(|e| fail(e.to_string()))?;
        let phone_statement = match &phone {
            Some(p) => {
                Some(sign_donation_phone(&m.id, p, signing).map_err(|e| fail(e.to_string()))?)
            },
            None => None,
        };
        // Self-check against the public half.
        let vk = signing.verifying_key();
        let back = verify_donation_method(&statement, &vk)
            .map_err(|e| fail(format!("internal: statement does not verify: {e}")))?;
        if back != method {
            return Err(fail("internal: statement does not round-trip".to_string()));
        }
        if let (Some(p), Some(wire)) = (&phone, &phone_statement) {
            let got = verify_donation_phone(wire, &vk, &m.id)
                .map_err(|e| fail(format!("internal: phone does not verify: {e}")))?;
            if &got != p {
                return Err(fail("internal: phone does not round-trip".to_string()));
            }
        }
        methods.push(SignedMethod {
            id: m.id,
            countries,
            statement_b64: STANDARD.encode(&statement),
            phone_b64: phone_statement.map(|w| STANDARD.encode(w)),
            qr_png_b64: qr_png.map(|q| STANDARD.encode(q)),
        });
    }
    Ok(SignedFile { methods })
}

/// Read a QR image: a PNG of at most [`MAX_QR_BYTES`].
fn read_qr(path: &Path) -> Result<Vec<u8>, String> {
    let bytes = fs::read(path).map_err(|e| format!("qr_file `{}`: {e}", path.display()))?;
    if bytes.len() > MAX_QR_BYTES {
        return Err(format!("qr_file is larger than {MAX_QR_BYTES} bytes"));
    }
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("qr_file is not a PNG".to_string());
    }
    Ok(bytes)
}

/// Create a file mode `0600` (refusing to overwrite), for the seed.
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
                "seed file `{}` already exists - refusing to overwrite \
                 (move it aside to rotate the donations key)",
                path.display()
            )
        } else {
            format!("cannot create seed file `{}`: {e}", path.display())
        }
    })?;
    file.write_all(seed)
        .map_err(|e| format!("cannot write seed file `{}`: {e}", path.display()))
}

/// Write the signed file mode `0600` (it holds the phone number), replacing
/// an older one.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut file = opts
        .open(path)
        .map_err(|e| format!("cannot create `{}`: {e}", path.display()))?;
    file.write_all(bytes)
        .map_err(|e| format!("cannot write `{}`: {e}", path.display()))
}

/// Read a 32-byte seed file.
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

/// Current Unix time in milliseconds.
fn now_unix_millis() -> Result<i64, String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_string())?
        .as_millis();
    i64::try_from(millis).map_err(|_| "current time overflows i64 milliseconds".to_string())
}

/// Parse `--flag value` pairs.
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

/// The first value of `name`.
fn flag<'a>(flags: &'a [(String, String)], name: &str) -> Option<&'a str> {
    flags
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// Refuse a flag not in `allowed`.
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

    fn unique_tmpdir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("lakko-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    const FILE: &str = r#"{"methods": [
        {"id": "sbp", "kind": "sbp", "link": "https://pay.example/sbp", "qr_file": "sbp.png",
         "phone": "+7 900 000-00-00", "countries": "RU,BY"},
        {"id": "bmc", "kind": "bmc", "link": "https://buymeacoffee.com/x", "countries": "!RU"},
        {"id": "usdt", "kind": "crypto", "coin": "USDT", "network": "TRON (TRC-20)", "address": "Tabc"},
        {"id": "btc", "kind": "crypto", "coin": "Bitcoin", "network": "Bitcoin", "address": "bc1q", "icon": "btc"}
    ]}"#;

    #[test]
    fn keygen_then_sign_files_round_trip() {
        let dir = unique_tmpdir("donations-roundtrip");
        let seed = dir.join("key.seed");
        let input = dir.join("methods.json");
        let out = dir.join("signed.json");
        let png = b"\x89PNG\r\n\x1a\nqr-bytes".to_vec();
        fs::write(dir.join("sbp.png"), &png).unwrap();
        fs::write(&input, FILE).unwrap();
        cmd_keygen(&["--out".into(), seed.display().to_string()]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&seed).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        cmd_sign(&[
            "--in".into(),
            input.display().to_string(),
            "--key".into(),
            seed.display().to_string(),
            "--out".into(),
            out.display().to_string(),
        ])
        .unwrap();
        let signing = read_signing_key(&seed).unwrap();
        let vk = signing.verifying_key();
        let signed: SignedFile = serde_json::from_slice(&fs::read(&out).unwrap()).unwrap();
        assert_eq!(signed.methods.len(), 4);
        let sbp = &signed.methods[0];
        assert_eq!(sbp.countries.as_deref(), Some("RU,BY"));
        let m = verify_donation_method(&STANDARD.decode(&sbp.statement_b64).unwrap(), &vk).unwrap();
        assert_eq!(m.qr_sha256, Some(sha256(&png)));
        assert!(m.has_phone);
        assert_eq!(
            STANDARD.decode(sbp.qr_png_b64.as_ref().unwrap()).unwrap(),
            png
        );
        let phone = STANDARD.decode(sbp.phone_b64.as_ref().unwrap()).unwrap();
        assert_eq!(
            verify_donation_phone(&phone, &vk, "sbp").unwrap(),
            "+7 900 000-00-00"
        );
        let usdt = &signed.methods[2];
        assert!(usdt.countries.is_none() && usdt.phone_b64.is_none());
        let m =
            verify_donation_method(&STANDARD.decode(&usdt.statement_b64).unwrap(), &vk).unwrap();
        assert_eq!(m.kind, DonationKind::Crypto);
        assert_eq!(m.icon, DonationIcon::None);
        let btc = &signed.methods[3];
        let m = verify_donation_method(&STANDARD.decode(&btc.statement_b64).unwrap(), &vk).unwrap();
        assert_eq!(m.icon, DonationIcon::Btc);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bad_input_is_refused() {
        let dir = unique_tmpdir("donations-bad");
        fs::write(dir.join("big.png"), {
            let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
            v.resize(MAX_QR_BYTES + 1, 0);
            v
        })
        .unwrap();
        fs::write(dir.join("gif.png"), b"GIF89a").unwrap();
        let sk = SigningKey::from_seed(&[7; 32]);
        for json in [
            r#"{"methods": [{"id": "a", "kind": "bmc", "link": "http://x"}]}"#,
            r#"{"methods": [{"id": "a", "kind": "bmc"}]}"#,
            r#"{"methods": [{"id": "a", "kind": "crypto", "coin": "BTC"}]}"#,
            r#"{"methods": [{"id": "A", "kind": "bmc", "link": "https://x"}]}"#,
            r#"{"methods": [{"id": "a", "kind": "paypal", "link": "https://x"}]}"#,
            r#"{"methods": [{"id": "a", "kind": "bmc", "link": "https://x", "extra": 1}]}"#,
            r#"{"methods": [{"id": "a", "kind": "bmc", "link": "https://x", "countries": "RU,!BY"}]}"#,
            r#"{"methods": [{"id": "a", "kind": "sbp", "qr_file": "big.png"}]}"#,
            r#"{"methods": [{"id": "a", "kind": "sbp", "qr_file": "gif.png"}]}"#,
            r#"{"methods": [{"id": "a", "kind": "sbp", "phone": "call me"}]}"#,
            r#"{"methods": [{"id": "a", "kind": "bmc", "link": "https://x", "icon": "btc"}]}"#,
            r#"{"methods": [{"id": "a", "kind": "crypto", "coin": "c", "network": "n", "address": "x", "icon": "usdt"}]}"#,
            r#"{"methods": [{"id": "a", "kind": "bmc", "link": "https://x"},
                            {"id": "a", "kind": "bmc", "link": "https://y"}]}"#,
        ] {
            assert!(sign_file(json, &dir, &sk, 1).is_err(), "{json}");
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn keygen_refuses_to_overwrite() {
        let dir = unique_tmpdir("donations-nooverwrite");
        let seed = dir.join("key.seed");
        cmd_keygen(&["--out".into(), seed.display().to_string()]).unwrap();
        let err = cmd_keygen(&["--out".into(), seed.display().to_string()]).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        fs::remove_dir_all(&dir).ok();
    }
}
