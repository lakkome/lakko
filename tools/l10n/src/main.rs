// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `lakko-l10n` — the catalog tool of ADR-0061.
//!
//! - `check` — validate the source catalog and every translation. Fails on
//!   any defect (bad syntax, unknown key, arguments or plural forms that do
//!   not fit the source, a source key without a translator comment). Prints
//!   per language how many keys are missing and how many security-critical
//!   keys still wait for review.
//! - `compile` — write what the clients embed: `source.json`, one compiled
//!   `<lang>.json` per translation, and `bundle-payload.json` (every
//!   language, security-critical keys left out) for `sign`.
//! - `android-locales` — write Android's `res/xml/locales_config.xml` from
//!   the supported-language table, so the per-app language list in system
//!   Settings can never drift from `lakko_l10n::supported_languages`.
//! - `ios-infoplist` — write `<lproj>.lproj/InfoPlist.strings` for English and
//!   every supported language: the iOS permission prompts (`NS*UsageDescription`),
//!   which the system shows itself and so cannot go through the in-app
//!   catalog. A language without a translation of a prompt gets the English.
//! - `keygen` / `sign` — the offline translation-publisher key and the
//!   signed bundle. The key is dedicated: not the ADR-0038 release key.
//!
//! A maintainer CLI, not a service: human-readable output on stdout/stderr
//! rather than `tracing`, and it may read the clock.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_crypto::signing::SigningKey;
use lakko_l10n::bundle::{bundle_payload, AcceptancePolicy};
use lakko_l10n::catalog::{compiled_messages, CompileReport};
use lakko_l10n::{
    accept_bundle, compile_translation, sign_bundle, supported_languages, Message, SourceCatalog,
    SOURCE_LANGUAGE,
};

const DEFAULT_DIR: &str = "l10n/catalog";
const DEFAULT_SEED_FILE: &str = "translation-signing-key.seed";
const DEFAULT_BUNDLE_OUT: &str = "translation-bundle.bin";
const SEED_LEN: usize = 32;

const USAGE: &str = "\
lakko-l10n — interface catalog tool (ADR-0061)

usage:
  lakko-l10n check   [--dir <catalog dir>]
  lakko-l10n compile [--dir <catalog dir>] --out <dir>
  lakko-l10n android-locales --out <locales_config.xml>
  lakko-l10n ios-infoplist [--dir <catalog dir>] --out <dir of .lproj folders>
  lakko-l10n keygen  [--out <seedfile>]
  lakko-l10n sign    --payload <bundle-payload.json> --version <N> --min-release <N>
                     [--key <seedfile>] [--out <path>]";

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
        "check" => cmd_check(rest),
        "compile" => cmd_compile(rest),
        "android-locales" => cmd_android_locales(rest),
        "ios-infoplist" => cmd_ios_infoplist(rest),
        "keygen" => cmd_keygen(rest),
        "sign" => cmd_sign(rest),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(())
        },
        other => Err(format!("unknown subcommand `{other}`")),
    }
}

struct Loaded {
    source_bytes: Vec<u8>,
    source: SourceCatalog,
    /// (language code, compiled bytes, report), in file-name order.
    translations: Vec<(String, Vec<u8>, CompileReport)>,
}

fn load(dir: &Path) -> Result<Loaded, String> {
    let source_path = dir.join(format!("{SOURCE_LANGUAGE}.json"));
    let source_bytes = fs::read(&source_path)
        .map_err(|e| format!("cannot read `{}`: {e}", source_path.display()))?;
    let source = SourceCatalog::from_json(&source_bytes)
        .map_err(|e| format!("`{}`: {e}", source_path.display()))?;
    let uncommented: Vec<&String> = source
        .messages()
        .iter()
        .filter(|(_, m)| m.comment.trim().is_empty())
        .map(|(k, _)| k)
        .collect();
    if !uncommented.is_empty() {
        return Err(format!(
            "source keys without a translator comment: {uncommented:?}"
        ));
    }

    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| format!("cannot list `{}`: {e}", dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json") && *p != source_path)
        .collect();
    paths.sort();

    let mut translations = Vec::new();
    for path in paths {
        let bytes =
            fs::read(&path).map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
        let (compiled, report) = compile_translation(&source, &bytes)
            .map_err(|e| format!("`{}`: {e}", path.display()))?;
        let (code, _) =
            compiled_messages(&compiled).map_err(|e| format!("`{}`: {e}", path.display()))?;
        let expected = format!("{code}.json");
        if path.file_name().and_then(|n| n.to_str()) != Some(expected.as_str()) {
            return Err(format!(
                "`{}` declares language `{code}`; name it `{expected}`",
                path.display()
            ));
        }
        translations.push((code, compiled, report));
    }
    Ok(Loaded {
        source_bytes,
        source,
        translations,
    })
}

fn cmd_check(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    reject_unknown(&flags, &["dir"])?;
    let dir = PathBuf::from(flag(&flags, "dir").unwrap_or(DEFAULT_DIR));
    let loaded = load(&dir)?;
    println!(
        "source: {} keys, version {}",
        loaded.source.messages().len(),
        loaded.source.version()
    );
    for (code, _, report) in &loaded.translations {
        println!(
            "{code}: {} missing, {} security-critical awaiting review",
            report.missing.len(),
            report.unreviewed_critical.len()
        );
    }
    println!("OK");
    Ok(())
}

fn cmd_compile(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    reject_unknown(&flags, &["dir", "out"])?;
    let dir = PathBuf::from(flag(&flags, "dir").unwrap_or(DEFAULT_DIR));
    let out = PathBuf::from(flag(&flags, "out").ok_or("missing required flag --out")?);
    let loaded = load(&dir)?;
    fs::create_dir_all(&out).map_err(|e| format!("cannot create `{}`: {e}", out.display()))?;

    write(&out.join("source.json"), &loaded.source_bytes)?;
    let mut payload_langs: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (code, compiled, _) in &loaded.translations {
        write(&out.join(format!("{code}.json")), compiled)?;
        let (_, strings) = compiled_messages(compiled).map_err(|e| e.to_string())?;
        let updatable: BTreeMap<String, String> = strings
            .into_iter()
            .filter(|(k, _)| !loaded.source.is_security_critical(k))
            .collect();
        payload_langs.insert(code.clone(), updatable);
    }
    let payload = bundle_payload(&payload_langs).map_err(|e| e.to_string())?;
    write(&out.join("bundle-payload.json"), &payload)?;
    println!(
        "compiled source + {} translations into {}",
        loaded.translations.len(),
        out.display()
    );
    Ok(())
}

fn cmd_android_locales(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    reject_unknown(&flags, &["out"])?;
    let out = PathBuf::from(flag(&flags, "out").ok_or("missing required flag --out")?);
    write(&out, android_locales_config().as_bytes())?;
    println!("wrote {}", out.display());
    Ok(())
}

/// The Android 13+ per-app language list, one `<locale>` per supported
/// language in table order. The codes are BCP 47 tags Android accepts as
/// they are (`pt-BR`, `zh-Hans`).
fn android_locales_config() -> String {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <!--\n\
         \x20 SPDX-License-Identifier: AGPL-3.0-or-later\n\
         \x20 SPDX-FileCopyrightText: 2026 Lakko Contributors\n\
         \n\
         \x20 ADR-0061: the interface languages Lakko ships, for the Android 13+\n\
         \x20 per-app language setting. Generated by `just l10n-compile` from\n\
         \x20 crates/lakko-l10n/src/language.rs; do not edit by hand.\n\
         -->\n\
         <locale-config xmlns:android=\"http://schemas.android.com/apk/res/android\">\n",
    );
    for language in supported_languages() {
        xml.push_str("    <locale android:name=\"");
        xml.push_str(language.code);
        xml.push_str("\" />\n");
    }
    xml.push_str("</locale-config>\n");
    xml
}

/// The iOS Info.plist keys the system shows in its own permission alerts,
/// each with the catalog key that carries its text. Order is the order in
/// the written file.
const IOS_INFOPLIST_KEYS: &[(&str, &str)] = &[
    ("NSCameraUsageDescription", "ios.permission.camera"),
    ("NSMicrophoneUsageDescription", "ios.permission.microphone"),
    (
        "NSPhotoLibraryAddUsageDescription",
        "ios.permission.photo_library_add",
    ),
    ("NSFaceIDUsageDescription", "ios.permission.face_id"),
];

fn cmd_ios_infoplist(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    reject_unknown(&flags, &["dir", "out"])?;
    let dir = PathBuf::from(flag(&flags, "dir").unwrap_or(DEFAULT_DIR));
    let out = PathBuf::from(flag(&flags, "out").ok_or("missing required flag --out")?);
    let loaded = load(&dir)?;

    let mut english: BTreeMap<String, String> = BTreeMap::new();
    for (_, key) in IOS_INFOPLIST_KEYS {
        let message = loaded
            .source
            .messages()
            .get(*key)
            .ok_or_else(|| format!("source catalog has no `{key}`"))?;
        english.insert((*key).to_string(), plain(key, &message.text)?);
    }
    let mut translated: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (code, compiled, _) in &loaded.translations {
        let (_, strings) = compiled_messages(compiled).map_err(|e| e.to_string())?;
        translated.insert(code.clone(), strings);
    }

    let mut written = 0usize;
    for language in supported_languages() {
        let lproj = apple_lproj(language.code)?;
        let mut entries = Vec::new();
        for (plist_key, key) in IOS_INFOPLIST_KEYS {
            let text = match translated
                .get(language.code)
                .and_then(|strings| strings.get(*key))
            {
                Some(text) => plain(key, text)?,
                None => english.get(*key).cloned().unwrap_or_default(),
            };
            entries.push((*plist_key, text));
        }
        let folder = out.join(format!("{lproj}.lproj"));
        fs::create_dir_all(&folder)
            .map_err(|e| format!("cannot create `{}`: {e}", folder.display()))?;
        write(
            &folder.join("InfoPlist.strings"),
            infoplist_strings(language.code, &entries).as_bytes(),
        )?;
        written += 1;
    }
    println!(
        "wrote {written} InfoPlist.strings files into {}",
        out.display()
    );
    Ok(())
}

/// The plain text of a catalog message; a permission prompt takes no
/// arguments, since iOS shows the string as it is.
fn plain(key: &str, text: &str) -> Result<String, String> {
    Message::parse(text)
        .map_err(|e| format!("`{key}`: {e}"))?
        .plain_text()
        .ok_or_else(|| format!("`{key}` is shown by iOS itself and must not take arguments"))
}

/// The `.lproj` folder name Apple uses for one of Lakko's language codes.
/// Listed explicitly so a new language in the table is a decision here, not
/// a silent guess.
fn apple_lproj(code: &str) -> Result<&'static str, String> {
    Ok(match code {
        "en" => "en",
        "ar" => "ar",
        "es" => "es",
        "fa" => "fa",
        "he" => "he",
        "hi" => "hi",
        "pt-BR" => "pt-BR",
        "ru" => "ru",
        "tr" => "tr",
        "uk" => "uk",
        "zh-Hans" => "zh-Hans",
        other => {
            return Err(format!(
                "no Apple .lproj name known for `{other}`; add it to apple_lproj"
            ))
        },
    })
}

/// One `InfoPlist.strings` file (UTF-8, which Xcode accepts).
fn infoplist_strings(code: &str, entries: &[(&str, String)]) -> String {
    let mut out = format!(
        "/*\n\
         \x20 SPDX-License-Identifier: AGPL-3.0-or-later\n\
         \x20 SPDX-FileCopyrightText: 2026 Lakko Contributors\n\
         \n\
         \x20 ADR-0061: iOS permission prompts, language `{code}`. Generated by\n\
         \x20 `just l10n-compile` from l10n/catalog; do not edit by hand.\n\
         */\n"
    );
    for (key, text) in entries {
        out.push('\n');
        out.push('"');
        out.push_str(&escape_strings(key));
        out.push_str("\" = \"");
        out.push_str(&escape_strings(text));
        out.push_str("\";\n");
    }
    out
}

/// Escape a value for the old-style `.strings` syntax: a backslash, a double
/// quote and the control characters, which would otherwise end the string
/// or break the file.
fn escape_strings(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                for unit in c.encode_utf16(&mut [0u16; 2]) {
                    out.push_str("\\U");
                    for shift in [12u16, 8, 4, 0] {
                        let nibble = u32::from((*unit >> shift) & 0xF);
                        out.push(
                            char::from_digit(nibble, 16).map_or('0', |d| d.to_ascii_uppercase()),
                        );
                    }
                }
            },
            c => out.push(c),
        }
    }
    out
}

fn cmd_keygen(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    reject_unknown(&flags, &["out"])?;
    let out = flag(&flags, "out").unwrap_or(DEFAULT_SEED_FILE);
    let signing = SigningKey::generate();
    write_seed_0600(Path::new(out), &signing.to_seed())?;
    println!("Translation signing keypair generated.");
    println!();
    println!("  private seed written to : {out}  (mode 0600 - keep this OFFLINE, never commit)");
    println!(
        "  public verifying key    : {}",
        URL_SAFE_NO_PAD.encode(signing.verifying_key().to_bytes())
    );
    println!();
    println!("Pin the public key in the iOS and Android build configs. It is not the");
    println!("release-manifest key; keep the two apart.");
    Ok(())
}

fn cmd_sign(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    reject_unknown(&flags, &["payload", "version", "min-release", "key", "out"])?;
    let payload_path = flag(&flags, "payload").ok_or("missing required flag --payload")?;
    let version = parse_u64(&flags, "version")?;
    let min_release = parse_u64(&flags, "min-release")?;
    let key_path = flag(&flags, "key").unwrap_or(DEFAULT_SEED_FILE);
    let out = flag(&flags, "out").unwrap_or(DEFAULT_BUNDLE_OUT);

    let payload =
        fs::read(payload_path).map_err(|e| format!("cannot read `{payload_path}`: {e}"))?;
    let signing = read_signing_key(Path::new(key_path))?;
    let signed_at = now_unix_millis()?;
    let bundle = sign_bundle(&signing, version, min_release, signed_at, &payload)
        .map_err(|e| e.to_string())?;

    // Self-check through the same acceptance path a client runs.
    accept_bundle(
        &bundle,
        &signing.verifying_key(),
        AcceptancePolicy {
            current_release: min_release,
            built_in_version: 0,
            last_accepted: None,
        },
    )
    .map_err(|e| format!("internal: freshly signed bundle does not verify: {e}"))?;

    write(Path::new(out), &bundle)?;
    println!("Translation bundle signed ({} bytes).", bundle.len());
    println!("  bundle_version = {version}");
    println!("  min_release    = {min_release}");
    println!("  written to     = {out}");
    println!();
    println!("The version must be greater than the catalog version compiled into every");
    println!("build that should accept it.");
    Ok(())
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    fs::write(path, bytes).map_err(|e| format!("cannot write `{}`: {e}", path.display()))
}

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
                "seed file `{}` already exists - refusing to overwrite",
                path.display()
            )
        } else {
            format!("cannot create seed file `{}`: {e}", path.display())
        }
    })?;
    file.write_all(seed)
        .map_err(|e| format!("cannot write seed file `{}`: {e}", path.display()))
}

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

fn now_unix_millis() -> Result<i64, String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_string())?
        .as_millis();
    i64::try_from(millis).map_err(|_| "current time overflows i64 milliseconds".to_string())
}

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

fn flag<'a>(flags: &'a [(String, String)], name: &str) -> Option<&'a str> {
    flags
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

fn parse_u64(flags: &[(String, String)], name: &str) -> Result<u64, String> {
    let value = flag(flags, name).ok_or_else(|| format!("missing required flag --{name}"))?;
    value
        .parse::<u64>()
        .map_err(|_| format!("--{name} must be a non-negative integer, got `{value}`"))
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
    use super::{escape_strings, infoplist_strings};

    #[test]
    fn strings_escaping_keeps_the_file_well_formed() {
        assert_eq!(escape_strings("plain"), "plain");
        assert_eq!(escape_strings(r#"say "hi""#), r#"say \"hi\""#);
        assert_eq!(escape_strings(r"a\b"), r"a\\b");
        assert_eq!(escape_strings("a\nb\tc\rd"), r"a\nb\tc\rd");
        assert_eq!(escape_strings("\u{1}"), r"\U0001");
        // Apostrophes and non-ASCII text pass through; the file is UTF-8.
        assert_eq!(
            escape_strings("Lakko's камера カメラ"),
            "Lakko's камера カメラ"
        );
    }

    #[test]
    fn strings_file_has_one_line_per_key() {
        let file = infoplist_strings(
            "ru",
            &[("NSCameraUsageDescription", "Камера \"для\" QR".to_string())],
        );
        assert!(file.starts_with("/*\n"));
        assert!(file.contains("SPDX-License-Identifier: AGPL-3.0-or-later"));
        assert!(file.ends_with("\n\"NSCameraUsageDescription\" = \"Камера \\\"для\\\" QR\";\n"));
    }
}
