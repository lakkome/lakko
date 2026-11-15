// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The ways to donate to Lakko (`docs/ux/donations.md` section 3).
//!
//! * `GET /discovery/donate` - the methods allowed for the request's
//!   country, without phone numbers;
//! * `GET /discovery/donate/{id}/qr` - a method's QR image (PNG);
//! * `POST /discovery/donate/{id}/phone` with `{"action":"reveal"}` - a
//!   method's phone number. A POST so crawlers and link previews, which
//!   follow GET links, do not collect it.
//!
//! No session, no token; the per-IP bucket of the other anonymous routes.
//! The country comes from the same city database `GET /discovery/locate`
//! reads, for the same nginx-written source address. Nothing is stored and
//! no log line names the address, the country or the method asked for.
//!
//! The methods are read once at startup from the JSON file named by
//! [`DONATIONS_FILE_ENV`]:
//!
//! ```json
//! {"methods": [
//!   {"id": "sbp", "kind": "sbp", "link": "https://...", "qr_file": "/etc/lakko/donate/sbp.png",
//!    "phone": "+7 900 000-00-00", "countries": "RU,BY"},
//!   {"id": "revolut", "kind": "revolut", "link": "https://...", "countries": "!RU,!BY"},
//!   {"id": "usdt", "kind": "crypto", "coin": "USDT", "network": "TRON (TRC-20)", "address": "T..."}
//! ]}
//! ```
//!
//! `countries` is a list of allowed ISO 3166-1 alpha-2 codes, or a list of
//! refused ones each prefixed with `!`, or absent (everywhere). An unknown
//! country passes a refuse-list and fails an allow-list, as in the previous
//! app. Unset, unreadable or malformed file: the routes answer an empty list
//! and startup logs one warning (a donation page is not worth taking
//! discovery down for).

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path as UrlPath, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::AppState;

/// Environment variable naming the donation methods file.
pub(crate) const DONATIONS_FILE_ENV: &str = "LAKKO_DISCOVERY_DONATIONS_FILE";

/// Most methods the file may list.
const MAX_METHODS: usize = 16;
/// Largest QR image accepted, in bytes.
const MAX_QR_BYTES: u64 = 512 * 1024;
/// Longest link, coin, network, address or phone, in bytes.
const MAX_FIELD_BYTES: usize = 512;

/// What kind of method a card shows. The client picks its description line
/// from this, so the set is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    /// SBP / a Russian bank transfer.
    Sbp,
    /// Revolut.
    Revolut,
    /// Buy Me a Coffee.
    Bmc,
    /// A crypto wallet.
    Crypto,
}

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

/// One method as the file lists it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MethodFile {
    id: String,
    kind: Kind,
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
}

/// The file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DonationsFile {
    methods: Vec<MethodFile>,
}

/// A checked method held in memory.
#[derive(Debug, Clone)]
pub(crate) struct Method {
    id: String,
    kind: Kind,
    link: Option<String>,
    qr_png: Option<Arc<Vec<u8>>>,
    phone: Option<String>,
    countries: CountryRule,
    coin: Option<String>,
    network: Option<String>,
    address: Option<String>,
}

/// What `GET /discovery/donate` says about one method.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct MethodView {
    id: String,
    kind: Kind,
    #[serde(skip_serializing_if = "Option::is_none")]
    link: Option<String>,
    has_qr: bool,
    has_phone: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    coin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    network: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    address: Option<String>,
}

/// The configured methods, in file order.
#[derive(Debug, Clone, Default)]
pub(crate) struct Donations {
    methods: Vec<Method>,
}

/// A text field: trimmed, non-empty, bounded, no control character.
fn text(value: Option<String>, what: &str) -> Result<Option<String>, String> {
    let Some(value) = value else { return Ok(None) };
    let value = value.trim().to_string();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > MAX_FIELD_BYTES || value.chars().any(char::is_control) {
        return Err(format!("{what} is too long or holds a control character"));
    }
    Ok(Some(value))
}

impl Donations {
    /// Check the file's methods. `read_qr` reads a QR file; a test passes a
    /// fake. Any problem refuses the whole file, naming the method's position.
    fn from_file(
        file: DonationsFile,
        read_qr: &dyn Fn(&str) -> Result<Vec<u8>, String>,
    ) -> Result<Self, String> {
        if file.methods.len() > MAX_METHODS {
            return Err(format!("more than {MAX_METHODS} methods"));
        }
        let mut seen = HashSet::new();
        let mut methods = Vec::with_capacity(file.methods.len());
        for (index, m) in file.methods.into_iter().enumerate() {
            let fail = |e: String| format!("method {index}: {e}");
            let id_ok = !m.id.is_empty()
                && m.id.len() <= 32
                && m.id.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'
                });
            if !id_ok {
                return Err(fail("id must be 1-32 of a-z 0-9 - _".to_string()));
            }
            if !seen.insert(m.id.clone()) {
                return Err(fail("id appears twice".to_string()));
            }
            let link = text(m.link, "link").map_err(fail)?;
            if link.as_deref().is_some_and(|l| !l.starts_with("https://")) {
                return Err(fail("link must start with https://".to_string()));
            }
            let qr_png = match m
                .qr_file
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty())
            {
                Some(path) => Some(Arc::new(read_qr(path).map_err(fail)?)),
                None => None,
            };
            let method = Method {
                id: m.id,
                kind: m.kind,
                link,
                qr_png,
                phone: text(m.phone, "phone").map_err(fail)?,
                countries: CountryRule::parse(m.countries.as_deref().unwrap_or(""))
                    .map_err(fail)?,
                coin: text(m.coin, "coin").map_err(fail)?,
                network: text(m.network, "network").map_err(fail)?,
                address: text(m.address, "address").map_err(fail)?,
            };
            if method.kind == Kind::Crypto {
                if method.coin.is_none() || method.network.is_none() || method.address.is_none() {
                    return Err(fail(
                        "a crypto method needs coin, network and address".to_string(),
                    ));
                }
            } else if method.link.is_none() && method.qr_png.is_none() && method.phone.is_none() {
                return Err(fail("needs a link, a QR file or a phone".to_string()));
            }
            methods.push(method);
        }
        Ok(Self { methods })
    }

    /// Parse the file's JSON text.
    fn parse(
        json: &str,
        read_qr: &dyn Fn(&str) -> Result<Vec<u8>, String>,
    ) -> Result<Self, String> {
        let file: DonationsFile =
            serde_json::from_str(json).map_err(|e| format!("not a donations file: {e}"))?;
        Self::from_file(file, read_qr)
    }

    /// The methods a request from `country` may see, as the list route
    /// answers them.
    pub(crate) fn views_for(&self, country: Option<&str>) -> Vec<MethodView> {
        self.methods
            .iter()
            .filter(|m| m.countries.allows(country))
            .map(|m| MethodView {
                id: m.id.clone(),
                kind: m.kind,
                link: m.link.clone(),
                has_qr: m.qr_png.is_some(),
                has_phone: m.phone.is_some(),
                coin: m.coin.clone(),
                network: m.network.clone(),
                address: m.address.clone(),
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

/// Read a QR file, refusing one larger than [`MAX_QR_BYTES`] or not a PNG.
fn read_qr_file(path: &str) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(Path::new(path)).map_err(|e| format!("qr_file: {e}"))?;
    if meta.len() > MAX_QR_BYTES {
        return Err("qr_file is larger than 512 KiB".to_string());
    }
    let bytes = std::fs::read(Path::new(path)).map_err(|e| format!("qr_file: {e}"))?;
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("qr_file is not a PNG".to_string());
    }
    Ok(bytes)
}

/// Load the methods from the environment. Never fails: an unset variable or
/// a bad file leaves the routes answering an empty list, with one warning.
pub(crate) fn donations_from_env() -> Arc<Donations> {
    let Some(path) = std::env::var(DONATIONS_FILE_ENV)
        .ok()
        .filter(|p| !p.is_empty())
    else {
        tracing::info!("{DONATIONS_FILE_ENV} unset - GET /discovery/donate answers an empty list");
        return Arc::new(Donations::default());
    };
    let loaded = std::fs::read_to_string(Path::new(&path))
        .map_err(|e| format!("unreadable: {e}"))
        .and_then(|json| Donations::parse(&json, &read_qr_file));
    match loaded {
        Ok(donations) => {
            tracing::info!(
                methods = donations.methods.len(),
                "donation methods loaded for GET /discovery/donate"
            );
            Arc::new(donations)
        },
        Err(err) => {
            tracing::warn!(
                error = %err,
                "{DONATIONS_FILE_ENV} refused - GET /discovery/donate answers an empty list"
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

/// The phone route's answer.
#[derive(Debug, Serialize)]
struct PhoneResponse {
    phone: String,
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
        .and_then(|m| m.phone.clone())
    {
        Some(phone) => (
            [(header::CACHE_CONTROL, "no-store")],
            Json(PhoneResponse { phone }),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Methods by id, for tests that build state by hand.
#[cfg(test)]
pub(crate) fn test_donations(json: &str) -> Arc<Donations> {
    let fake_qr = |_: &str| Ok(b"\x89PNG\r\n\x1a\nqr".to_vec());
    Arc::new(Donations::parse(json, &fake_qr).expect("test donations file"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"{"methods": [
        {"id": "sbp", "kind": "sbp", "link": "https://pay.example/sbp", "qr_file": "/x.png",
         "phone": "+7 900 000-00-00", "countries": "RU,BY"},
        {"id": "revolut", "kind": "revolut", "link": "https://revolut.me/x", "countries": "!RU,!BY"},
        {"id": "bmc", "kind": "bmc", "link": "https://buymeacoffee.com/x"},
        {"id": "usdt", "kind": "crypto", "coin": "USDT", "network": "TRON (TRC-20)", "address": "Tabc"}
    ]}"#;

    fn ids(views: &[MethodView]) -> Vec<&str> {
        views.iter().map(|v| v.id.as_str()).collect()
    }

    #[test]
    fn the_country_rule_picks_the_methods() {
        let d = test_donations(FILE);
        assert_eq!(ids(&d.views_for(Some("RU"))), ["sbp", "bmc", "usdt"]);
        assert_eq!(ids(&d.views_for(Some("NL"))), ["revolut", "bmc", "usdt"]);
        // An unknown country fails an allow-list and passes a refuse-list.
        assert_eq!(ids(&d.views_for(None)), ["revolut", "bmc", "usdt"]);
    }

    #[test]
    fn the_list_never_carries_the_phone() {
        let d = test_donations(FILE);
        let json = serde_json::to_string(&d.views_for(Some("RU"))).unwrap();
        assert!(!json.contains("900"));
        assert!(json.contains(r#""has_phone":true"#));
        assert!(json.contains(r#""has_qr":true"#));
    }

    #[test]
    fn find_applies_the_country_rule() {
        let d = test_donations(FILE);
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
    fn a_bad_file_is_refused() {
        let fake_qr = |_: &str| Ok(b"\x89PNG\r\n\x1a\n".to_vec());
        let bad = [
            r#"{"methods": [{"id": "a", "kind": "bmc", "link": "http://x"}]}"#,
            r#"{"methods": [{"id": "a", "kind": "bmc"}]}"#,
            r#"{"methods": [{"id": "a", "kind": "crypto", "coin": "BTC"}]}"#,
            r#"{"methods": [{"id": "A", "kind": "bmc", "link": "https://x"}]}"#,
            r#"{"methods": [{"id": "a", "kind": "bmc", "link": "https://x"},
                            {"id": "a", "kind": "bmc", "link": "https://y"}]}"#,
            r#"{"methods": [{"id": "a", "kind": "paypal", "link": "https://x"}]}"#,
            r#"{"methods": [{"id": "a", "kind": "bmc", "link": "https://x", "extra": 1}]}"#,
        ];
        for json in bad {
            assert!(Donations::parse(json, &fake_qr).is_err(), "{json}");
        }
    }

    #[test]
    fn a_qr_that_is_not_a_png_is_refused() {
        let dir = std::env::temp_dir().join(format!("lakko-donate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("qr.png");
        std::fs::write(&path, b"GIF89a").unwrap();
        assert!(read_qr_file(path.to_str().unwrap()).is_err());
        std::fs::write(&path, b"\x89PNG\r\n\x1a\nrest").unwrap();
        assert!(read_qr_file(path.to_str().unwrap()).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
