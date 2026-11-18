// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The ways to donate to Lakko (`docs/ux/donations.md` section 3).
//!
//! Every method is a statement signed OFFLINE by the donations key
//! (`lakko-attest`'s `donation` module, signed by `tools/donations`), and the
//! client shows only what verifies against the key pinned into its build.
//! This service passes the signed bytes through untouched; it decides only
//! which methods a request's country sees.
//!
//! * `GET /discovery/donate` -> `{"methods": [{"id", "statement_b64"}]}`, the
//!   methods allowed for the request's country;
//! * `GET /discovery/donate/{id}/qr` - a method's QR image (PNG), whose
//!   SHA-256 the method statement carries;
//! * `POST /discovery/donate/{id}/phone` with `{"action":"reveal"}` ->
//!   `{"statement_b64"}`, the method's signed phone statement. A POST so
//!   crawlers and link previews, which follow GET links, do not collect it.
//!
//! `statement_b64` is standard base64 with padding. No session, no token; the
//! per-IP bucket of the other anonymous routes. The country comes from the
//! same city database `GET /discovery/locate` reads. Nothing is stored and no
//! log line names the address, the country or a method.
//!
//! The signed file is read once at startup, from the systemd credential
//! `$CREDENTIALS_DIRECTORY/lakko-donations` when systemd provides it, else
//! from the path in [`DONATIONS_FILE_ENV`] (development):
//!
//! ```json
//! {"methods": [
//!   {"id": "sbp", "countries": "RU,BY", "statement_b64": "...",
//!    "phone_b64": "...", "qr_png_b64": "..."},
//!   {"id": "usdt", "statement_b64": "..."}
//! ]}
//! ```
//!
//! `countries` is unsigned: a list of allowed ISO 3166-1 alpha-2 codes, or a
//! list of refused ones each prefixed with `!`, or absent (everywhere). An
//! unknown country passes a refuse-list and fails an allow-list.
//!
//! Every statement is parsed and its fields checked; its id must be the
//! entry's id, an embedded QR must hash to the statement's `qr_sha256`, and a
//! phone statement must exist exactly when the method says `has_phone` and
//! name the same id. With [`DONATIONS_KEY_ENV`] set, every signature is also
//! verified (a server that cannot verify serves what a client would refuse).
//! Any problem refuses the whole file: the routes answer an empty list and
//! startup logs one warning (a donation page is not worth taking discovery
//! down for).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path as UrlPath, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use base64::Engine as _;
use lakko_attest::{DonationMethod, DonationPhone};
use lakko_crypto::hash::sha256;
use lakko_crypto::signing::VerifyingKey;
use serde::{Deserialize, Serialize};

use crate::AppState;

/// Environment variable naming the signed donations file (development; a
/// deployed unit hands it over as the `lakko-donations` credential).
pub(crate) const DONATIONS_FILE_ENV: &str = "LAKKO_DISCOVERY_DONATIONS_FILE";

/// Environment variable holding the donations public key (base64url, 32
/// bytes). Optional: when set, every statement is verified at startup.
pub(crate) const DONATIONS_KEY_ENV: &str = "LAKKO_DISCOVERY_DONATIONS_KEY";

/// The credential name the units load the signed file under.
const CREDENTIAL_NAME: &str = "lakko-donations";

/// Most methods the file may list.
const MAX_METHODS: usize = 16;
/// Largest QR image accepted, in bytes.
const MAX_QR_BYTES: usize = 128 * 1024;
/// Largest signed file accepted, in bytes.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// Which countries a method is offered in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CountryRule {
    /// Everywhere.
    Everywhere,
    /// Only these countries; an unknown country is refused.
    Only(HashSet<String>),
    /// Everywhere but these countries; an unknown country is allowed.
    Except(HashSet<String>),
}

impl CountryRule {
    /// Parse `RU,BY`, `!RU,!BY` or an empty string. A mix of the two forms
    /// or a token that is not two letters is an error.
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        let tokens: Vec<&str> = raw
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect();
        if tokens.is_empty() {
            return Ok(Self::Everywhere);
        }
        let refused = tokens.iter().filter(|t| t.starts_with('!')).count();
        if refused != 0 && refused != tokens.len() {
            return Err("countries mixes allowed and refused codes".to_string());
        }
        let mut set = HashSet::new();
        for token in tokens {
            let code = token.trim_start_matches('!').to_ascii_uppercase();
            if code.len() != 2 || !code.bytes().all(|b| b.is_ascii_uppercase()) {
                return Err("countries holds a code that is not two letters".to_string());
            }
            set.insert(code);
        }
        Ok(if refused == 0 {
            Self::Only(set)
        } else {
            Self::Except(set)
        })
    }

    /// Whether a request from `country` may see the method.
    pub(crate) fn allows(&self, country: Option<&str>) -> bool {
        match self {
            Self::Everywhere => true,
            Self::Only(set) => country.is_some_and(|c| set.contains(c)),
            Self::Except(set) => !country.is_some_and(|c| set.contains(c)),
        }
    }
}

/// One method as the signed file lists it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MethodFile {
    id: String,
    #[serde(default)]
    countries: Option<String>,
    statement_b64: String,
    #[serde(default)]
    phone_b64: Option<String>,
    #[serde(default)]
    qr_png_b64: Option<String>,
}

/// The signed file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DonationsFile {
    methods: Vec<MethodFile>,
}

/// A checked method held in memory: the signed bytes, re-encoded, plus the
/// unsigned country rule.
#[derive(Debug, Clone)]
pub(crate) struct Method {
    id: String,
    countries: CountryRule,
    statement_b64: String,
    phone_b64: Option<String>,
    qr_png: Option<Arc<Vec<u8>>>,
}

/// What `GET /discovery/donate` says about one method.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct MethodView {
    id: String,
    statement_b64: String,
}

/// The configured methods, in file order.
#[derive(Debug, Clone, Default)]
pub(crate) struct Donations {
    methods: Vec<Method>,
    /// Whether the signatures were verified at load.
    verified: bool,
}

/// Decode standard base64 (with or without padding).
fn b64(field: &str, what: &str) -> Result<Vec<u8>, String> {
    B64.decode(field.trim())
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(field.trim()))
        .map_err(|_| format!("{what} is not base64"))
}

impl Donations {
    /// Check the file's methods; with `key`, verify every signature too. Any
    /// problem refuses the whole file, naming the method's position (never
    /// its id or content).
    fn from_file(file: DonationsFile, key: Option<&VerifyingKey>) -> Result<Self, String> {
        if file.methods.len() > MAX_METHODS {
            return Err(format!("more than {MAX_METHODS} methods"));
        }
        let mut seen = HashSet::new();
        let mut methods = Vec::with_capacity(file.methods.len());
        for (index, m) in file.methods.into_iter().enumerate() {
            let fail = |e: String| format!("method {index}: {e}");
            if !seen.insert(m.id.clone()) {
                return Err(fail("id appears twice".to_string()));
            }
            let statement = b64(&m.statement_b64, "statement_b64").map_err(fail)?;
            let method = match key {
                Some(k) => DonationMethod::open(&statement, k),
                None => DonationMethod::decode_unverified(&statement),
            }
            .map_err(|e| fail(format!("statement refused ({})", e.code())))?;
            if method.id != m.id {
                return Err(fail("statement names another id".to_string()));
            }
            let qr_png = match (&m.qr_png_b64, method.qr_sha256) {
                (None, None) => None,
                (Some(raw), Some(hash)) => {
                    let png = b64(raw, "qr_png_b64").map_err(fail)?;
                    if png.len() > MAX_QR_BYTES {
                        return Err(fail("QR is larger than 128 KiB".to_string()));
                    }
                    if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
                        return Err(fail("QR is not a PNG".to_string()));
                    }
                    if sha256(&png) != hash {
                        return Err(fail("QR does not match the signed hash".to_string()));
                    }
                    Some(Arc::new(png))
                },
                _ => return Err(fail("QR and statement disagree".to_string())),
            };
            let phone_b64 = match (&m.phone_b64, method.has_phone) {
                (None, false) => None,
                (Some(raw), true) => {
                    let wire = b64(raw, "phone_b64").map_err(fail)?;
                    let phone = match key {
                        Some(k) => DonationPhone::open(&wire, k),
                        None => DonationPhone::decode_unverified(&wire),
                    }
                    .map_err(|e| fail(format!("phone refused ({})", e.code())))?;
                    if phone.id != m.id {
                        return Err(fail("phone names another id".to_string()));
                    }
                    Some(B64.encode(&wire))
                },
                _ => return Err(fail("phone and statement disagree".to_string())),
            };
            methods.push(Method {
                id: m.id,
                countries: CountryRule::parse(m.countries.as_deref().unwrap_or(""))
                    .map_err(fail)?,
                statement_b64: B64.encode(&statement),
                phone_b64,
                qr_png,
            });
        }
        Ok(Self {
            methods,
            verified: key.is_some(),
        })
    }

    /// Parse the file's JSON text.
    fn parse(json: &str, key: Option<&VerifyingKey>) -> Result<Self, String> {
        let file: DonationsFile =
            serde_json::from_str(json).map_err(|e| format!("not a donations file: {e}"))?;
        Self::from_file(file, key)
    }

    /// The methods a request from `country` may see, as the list route
    /// answers them.
    pub(crate) fn views_for(&self, country: Option<&str>) -> Vec<MethodView> {
        self.methods
            .iter()
            .filter(|m| m.countries.allows(country))
            .map(|m| MethodView {
                id: m.id.clone(),
                statement_b64: m.statement_b64.clone(),
            })
            .collect()
    }

    /// The method `id`, when a request from `country` may see it.
    fn find(&self, id: &str, country: Option<&str>) -> Option<&Method> {
        self.methods
            .iter()
            .find(|m| m.id == id && m.countries.allows(country))
    }
}

/// Where the signed file is read from: the systemd credential when systemd
/// provides one, else the development path. `exists` is injected for tests.
fn source_path(
    credentials_dir: Option<&str>,
    file_env: Option<&str>,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if let Some(dir) = credentials_dir.filter(|d| !d.is_empty()) {
        let path = Path::new(dir).join(CREDENTIAL_NAME);
        if exists(&path) {
            return Some(path);
        }
    }
    file_env.filter(|p| !p.is_empty()).map(PathBuf::from)
}

/// Parse the optional donations key (base64url, padding tolerated).
fn parse_key(raw: &str) -> Result<VerifyingKey, String> {
    let bytes = B64URL
        .decode(raw.trim().trim_end_matches('='))
        .map_err(|_| format!("{DONATIONS_KEY_ENV} is not base64url"))?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| format!("{DONATIONS_KEY_ENV} is not a key"))
}

/// Read the file, refusing one larger than [`MAX_FILE_BYTES`].
fn read_file(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("unreadable: {}", e.kind()))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err("larger than 1 MiB".to_string());
    }
    std::fs::read_to_string(path).map_err(|e| format!("unreadable: {}", e.kind()))
}

/// Load the methods from the environment. Never fails: no file or a bad file
/// leaves the routes answering an empty list, with one log line.
pub(crate) fn donations_from_env() -> Arc<Donations> {
    let credentials_dir = std::env::var("CREDENTIALS_DIRECTORY").ok();
    let file_env = std::env::var(DONATIONS_FILE_ENV).ok();
    let Some(path) = source_path(credentials_dir.as_deref(), file_env.as_deref(), &|p| {
        p.is_file()
    }) else {
        tracing::info!(
            "no {CREDENTIAL_NAME} credential and {DONATIONS_FILE_ENV} unset - \
             GET /discovery/donate answers an empty list"
        );
        return Arc::new(Donations::default());
    };
    let key = std::env::var(DONATIONS_KEY_ENV)
        .ok()
        .filter(|k| !k.trim().is_empty());
    let loaded =
        key.as_deref().map(parse_key).transpose().and_then(|key| {
            read_file(&path).and_then(|json| Donations::parse(&json, key.as_ref()))
        });
    match loaded {
        Ok(donations) => {
            tracing::info!(
                methods = donations.methods.len(),
                verified = donations.verified,
                "donation methods loaded for GET /discovery/donate"
            );
            if !donations.verified {
                tracing::warn!(
                    "{DONATIONS_KEY_ENV} unset - donation signatures were not checked here \
                     (clients check them)"
                );
            }
            Arc::new(donations)
        },
        Err(err) => {
            tracing::warn!(
                error = %err,
                "donations file refused - GET /discovery/donate answers an empty list"
            );
            Arc::new(Donations::default())
        },
    }
}

/// The request's country, from the city database.
fn country_of(state: &AppState, req: &Request<Body>) -> Option<String> {
    crate::rate_limit::source_ip(req)
        .and_then(|ip| state.city_lookup.locate(ip))
        .and_then(|place| place.country)
}

/// The list route's answer.
#[derive(Debug, Serialize)]
struct ListResponse {
    methods: Vec<MethodView>,
}

/// `GET /discovery/donate`.
pub(crate) async fn list(State(state): State<AppState>, req: Request<Body>) -> Response {
    let country = country_of(&state, &req);
    let methods = state.donations.views_for(country.as_deref());
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(ListResponse { methods }),
    )
        .into_response()
}

/// `GET /discovery/donate/{id}/qr`.
pub(crate) async fn qr(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    req: Request<Body>,
) -> Response {
    let country = country_of(&state, &req);
    match state
        .donations
        .find(&id, country.as_deref())
        .and_then(|m| m.qr_png.clone())
    {
        Some(png) => (
            [
                (header::CONTENT_TYPE, "image/png"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            png.as_ref().clone(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// The phone route's body.
#[derive(Debug, Deserialize)]
struct PhoneRequest {
    action: String,
}

/// The phone route's answer: the signed phone statement.
#[derive(Debug, Serialize)]
struct PhoneResponse {
    statement_b64: String,
}

/// `POST /discovery/donate/{id}/phone`.
pub(crate) async fn phone(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    req: Request<Body>,
) -> Response {
    let country = country_of(&state, &req);
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), 1024).await else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match serde_json::from_slice::<PhoneRequest>(&bytes) {
        Ok(body) if body.action == "reveal" => {},
        _ => return StatusCode::BAD_REQUEST.into_response(),
    }
    match state
        .donations
        .find(&id, country.as_deref())
        .and_then(|m| m.phone_b64.clone())
    {
        Some(statement_b64) => (
            [(header::CACHE_CONTROL, "no-store")],
            Json(PhoneResponse { statement_b64 }),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// The seed of the key the test fixtures are signed with.
#[cfg(test)]
pub(crate) const TEST_KEY_SEED: [u8; 32] = [0x5a; 32];

/// The fixture QR image.
#[cfg(test)]
pub(crate) const TEST_QR: &[u8] = b"\x89PNG\r\n\x1a\nqr";

/// A signed file entry for tests: the method plus, when it says so, its
/// phone and QR.
#[cfg(test)]
pub(crate) fn test_entry(
    method: &lakko_attest::DonationMethod,
    countries: Option<&str>,
    phone: Option<&str>,
) -> serde_json::Value {
    let sk = lakko_crypto::signing::SigningKey::from_seed(&TEST_KEY_SEED);
    let mut entry = serde_json::json!({
        "id": method.id,
        "statement_b64": B64.encode(method.sign(&sk).expect("valid fixture")),
    });
    if let Some(c) = countries {
        entry["countries"] = c.into();
    }
    if let Some(p) = phone {
        let wire = lakko_attest::sign_donation_phone(&method.id, p, &sk).expect("valid phone");
        entry["phone_b64"] = B64.encode(wire).into();
    }
    if method.qr_sha256.is_some() {
        entry["qr_png_b64"] = B64.encode(TEST_QR).into();
    }
    entry
}

/// The fixture methods: `sbp` for RU (link, QR, phone `+7 900`), `bmc`
/// outside RU, `usdt` everywhere.
#[cfg(test)]
pub(crate) fn test_methods() -> Vec<(lakko_attest::DonationMethod, Option<&'static str>)> {
    use lakko_attest::DonationKind;
    let base = lakko_attest::DonationMethod {
        signed_at_unix_millis: 1,
        id: String::new(),
        kind: DonationKind::Bmc,
        link: None,
        coin: None,
        network: None,
        address: None,
        qr_sha256: None,
        has_phone: false,
    };
    vec![
        (
            lakko_attest::DonationMethod {
                id: "sbp".into(),
                kind: DonationKind::Sbp,
                link: Some("https://pay.example/sbp".into()),
                qr_sha256: Some(sha256(TEST_QR)),
                has_phone: true,
                ..base.clone()
            },
            Some("RU"),
        ),
        (
            lakko_attest::DonationMethod {
                id: "bmc".into(),
                link: Some("https://buymeacoffee.com/x".into()),
                ..base.clone()
            },
            Some("!RU"),
        ),
        (
            lakko_attest::DonationMethod {
                id: "usdt".into(),
                kind: DonationKind::Crypto,
                coin: Some("USDT".into()),
                network: Some("TRON (TRC-20)".into()),
                address: Some("Tabc".into()),
                ..base
            },
            None,
        ),
    ]
}

/// The fixture file as JSON text.
#[cfg(test)]
pub(crate) fn test_file() -> String {
    let methods: Vec<serde_json::Value> = test_methods()
        .iter()
        .map(|(m, c)| test_entry(m, *c, m.has_phone.then_some("+7 900")))
        .collect();
    serde_json::json!({ "methods": methods }).to_string()
}

/// The fixture methods loaded with signatures verified, for tests that build
/// state by hand.
#[cfg(test)]
pub(crate) fn test_donations() -> Arc<Donations> {
    let key = lakko_crypto::signing::SigningKey::from_seed(&TEST_KEY_SEED).verifying_key();
    Arc::new(Donations::parse(&test_file(), Some(&key)).expect("test donations file"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::signing::SigningKey;

    fn ids(views: &[MethodView]) -> Vec<&str> {
        views.iter().map(|v| v.id.as_str()).collect()
    }

    fn test_key() -> VerifyingKey {
        SigningKey::from_seed(&TEST_KEY_SEED).verifying_key()
    }

    #[test]
    fn the_country_rule_picks_the_methods() {
        let d = test_donations();
        assert!(d.verified);
        assert_eq!(ids(&d.views_for(Some("RU"))), ["sbp", "usdt"]);
        assert_eq!(ids(&d.views_for(Some("NL"))), ["bmc", "usdt"]);
        // An unknown country fails an allow-list and passes a refuse-list.
        assert_eq!(ids(&d.views_for(None)), ["bmc", "usdt"]);
    }

    #[test]
    fn the_list_carries_the_signed_bytes_and_never_the_phone() {
        let d = test_donations();
        let views = d.views_for(Some("RU"));
        let json = serde_json::to_string(&views).unwrap();
        let sbp = d.find("sbp", Some("RU")).unwrap();
        assert!(!json.contains(sbp.phone_b64.as_deref().unwrap()));
        assert!(!json.contains("900"));
        let m = DonationMethod::open(&B64.decode(&views[0].statement_b64).unwrap(), &test_key())
            .unwrap();
        assert_eq!(m.id, "sbp");
        assert!(m.has_phone);
    }

    #[test]
    fn without_a_key_the_layout_is_still_checked() {
        let d = Donations::parse(&test_file(), None).unwrap();
        assert!(!d.verified);
        assert_eq!(d.methods.len(), 3);
        let other = SigningKey::from_seed(&[1; 32]).verifying_key();
        assert!(Donations::parse(&test_file(), Some(&other)).is_err());
    }

    #[test]
    fn find_applies_the_country_rule() {
        let d = test_donations();
        assert!(d.find("sbp", Some("RU")).is_some());
        assert!(d.find("sbp", Some("NL")).is_none());
        assert!(d.find("sbp", None).is_none());
        assert!(d.find("nope", Some("RU")).is_none());
    }

    #[test]
    fn country_rules_parse() {
        assert_eq!(CountryRule::parse("").unwrap(), CountryRule::Everywhere);
        assert!(matches!(
            CountryRule::parse("ru, by").unwrap(),
            CountryRule::Only(_)
        ));
        assert!(matches!(
            CountryRule::parse("!RU").unwrap(),
            CountryRule::Except(_)
        ));
        assert!(CountryRule::parse("RU,!BY").is_err());
        assert!(CountryRule::parse("RUS").is_err());
    }

    #[test]
    fn a_file_that_does_not_match_its_statements_is_refused() {
        let methods = test_methods();
        let (sbp, _) = &methods[0];
        let (bmc, _) = &methods[1];
        let key = test_key();
        let refuse = |entries: Vec<serde_json::Value>| {
            let json = serde_json::json!({ "methods": entries }).to_string();
            assert!(Donations::parse(&json, Some(&key)).is_err(), "{json}");
            assert!(Donations::parse(&json, None).is_err(), "{json}");
        };
        // The entry's id is not the statement's.
        let mut e = test_entry(bmc, None, None);
        e["id"] = "other".into();
        refuse(vec![e]);
        // A QR that does not hash to the signed value.
        let mut e = test_entry(sbp, None, Some("+7 900"));
        e["qr_png_b64"] = B64.encode(b"\x89PNG\r\n\x1a\nother").into();
        refuse(vec![e]);
        // A QR the statement does not mention, or a missing one it does.
        let mut e = test_entry(bmc, None, None);
        e["qr_png_b64"] = B64.encode(TEST_QR).into();
        refuse(vec![e]);
        let mut e = test_entry(sbp, None, Some("+7 900"));
        e.as_object_mut().unwrap().remove("qr_png_b64");
        refuse(vec![e]);
        // has_phone without a phone, a phone without has_phone, another id's phone.
        refuse(vec![test_entry(sbp, None, None)]);
        refuse(vec![test_entry(bmc, None, Some("+7 900"))]);
        let mut e = test_entry(sbp, None, Some("+7 900"));
        let foreign = lakko_attest::sign_donation_phone(
            "bmc",
            "+7 900",
            &SigningKey::from_seed(&TEST_KEY_SEED),
        )
        .unwrap();
        e["phone_b64"] = B64.encode(foreign).into();
        refuse(vec![e]);
        // Twice the same id; a damaged statement; an extra field.
        refuse(vec![
            test_entry(bmc, None, None),
            test_entry(bmc, None, None),
        ]);
        let mut e = test_entry(bmc, None, None);
        e["statement_b64"] = B64.encode([1u8, 2, 3]).into();
        refuse(vec![e]);
        let mut e = test_entry(bmc, None, None);
        e["kind"] = "bmc".into();
        refuse(vec![e]);
    }

    #[test]
    fn the_credential_comes_before_the_development_path() {
        let yes = |_: &Path| true;
        let no = |_: &Path| false;
        assert_eq!(
            source_path(Some("/run/credentials/x"), Some("/dev.json"), &yes),
            Some(PathBuf::from("/run/credentials/x/lakko-donations"))
        );
        assert_eq!(
            source_path(Some("/run/credentials/x"), Some("/dev.json"), &no),
            Some(PathBuf::from("/dev.json"))
        );
        assert_eq!(source_path(None, None, &yes), None);
        assert_eq!(source_path(Some(""), Some(""), &yes), None);
    }

    #[test]
    fn the_key_parses_as_base64url() {
        let raw = SigningKey::from_seed(&TEST_KEY_SEED)
            .verifying_key()
            .to_bytes();
        assert!(parse_key(&B64URL.encode(raw)).is_ok());
        assert!(parse_key("not a key").is_err());
        assert!(parse_key(&B64URL.encode([1u8; 8])).is_err());
    }
}
