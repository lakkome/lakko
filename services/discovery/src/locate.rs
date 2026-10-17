// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `GET /discovery/locate` — which city the request's own source address
//! belongs to ([ADR-0080](../../../docs/adr/0080-device-cards.md) section 4).
//!
//! A device asks before it writes its device card, so the account's other
//! devices can show "Almere, Netherlands" on the Devices screen. The route:
//!
//! * takes **no session, no token and no account**, and reads nothing from the
//!   request but its source address (the same nginx-written address the
//!   per-IP limiter trusts, [`crate::rate_limit::source_ip`]);
//! * answers `{"city": "...", "country": "NL"}`, a partial answer when the
//!   database knows only the country, or `{}`;
//! * **keeps nothing**: no table, no cache, no counter beyond the in-memory
//!   per-address bucket every anonymous discovery route has;
//! * **writes no log line naming the address or the answer**. The request
//!   span the service's `TraceLayer` writes carries the method and path only,
//!   and this handler logs nothing at all.
//!
//! The server already sees the address of every request, so the lookup tells
//! it nothing new. What must not exist is a stored link between an account, a
//! device and a place, and none is created here: the answer goes only into the
//! end-to-end encrypted device card.
//!
//! The database is DB-IP City Lite in MaxMind format (CC BY 4.0), read with
//! the `maxminddb` crate from the path in [`CITY_DB_ENV`]. Unset, missing or
//! unreadable: the route answers `{}` and startup logs one warning, since a
//! missing city is cosmetic and must not take discovery down at a monthly
//! database refresh.

use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use crate::AppState;

/// Environment variable naming the city database file.
pub(crate) const CITY_DB_ENV: &str = "LAKKO_DISCOVERY_CITY_DB";

/// Longest city name forwarded, in bytes. The device card's own bound
/// (`lakko_contacts::device_card::DEVICE_CARD_CITY_MAX_BYTES`); a longer name
/// is dropped rather than cut here, and the client normalizes anyway.
const MAX_CITY_BYTES: usize = lakko_contacts::device_card::DEVICE_CARD_CITY_MAX_BYTES;

/// What the database says about an address. Either half may be absent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Place {
    /// English city name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) city: Option<String>,
    /// ISO 3166-1 alpha-2 country code, upper case.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) country: Option<String>,
}

/// A city database. A trait so tests use a fake instead of a database file.
pub(crate) trait CityLookup: Send + Sync {
    /// The place `ip` belongs to, or `None`.
    fn locate(&self, ip: IpAddr) -> Option<Place>;
}

/// The lookup used when no database is configured: knows nothing.
pub(crate) struct NoCityLookup;

impl CityLookup for NoCityLookup {
    fn locate(&self, _ip: IpAddr) -> Option<Place> {
        None
    }
}

/// A MaxMind-format city database held in memory.
pub(crate) struct MmdbCityLookup {
    reader: maxminddb::Reader<Vec<u8>>,
}

impl MmdbCityLookup {
    /// Read the database file at `path`.
    ///
    /// # Errors
    ///
    /// The `maxminddb` error for an unreadable or malformed file.
    pub(crate) fn open(path: &Path) -> Result<Self, maxminddb::MaxMindDbError> {
        Ok(Self {
            reader: maxminddb::Reader::open_readfile(path)?,
        })
    }
}

impl CityLookup for MmdbCityLookup {
    fn locate(&self, ip: IpAddr) -> Option<Place> {
        let found = self.reader.lookup(ip).ok()?;
        let city: maxminddb::geoip2::City<'_> = found.decode().ok()??;
        sanitize(city.city.names.english, city.country.iso_code)
    }
}

/// Keep only what a device card can carry: a city of printable text within
/// the card's bound, and a two-letter country code. `None` when neither half
/// survives.
fn sanitize(city: Option<&str>, country: Option<&str>) -> Option<Place> {
    let city = city
        .map(str::trim)
        .filter(|c| !c.is_empty() && c.len() <= MAX_CITY_BYTES)
        .filter(|c| !c.chars().any(char::is_control))
        .map(str::to_string);
    let country = country
        .map(|c| c.trim().to_ascii_uppercase())
        .filter(|c| c.len() == 2 && c.bytes().all(|b| b.is_ascii_uppercase()));
    if city.is_none() && country.is_none() {
        None
    } else {
        Some(Place { city, country })
    }
}

/// Build the lookup from the environment. Never fails: an unset variable or
/// an unreadable file leaves the route answering `{}`, with one warning at
/// startup that names neither an address nor the file's contents.
pub(crate) fn city_lookup_from_env() -> Arc<dyn CityLookup> {
    let Some(path) = std::env::var(CITY_DB_ENV).ok().filter(|p| !p.is_empty()) else {
        tracing::warn!("{CITY_DB_ENV} unset - GET /discovery/locate answers {{}}");
        return Arc::new(NoCityLookup);
    };
    match MmdbCityLookup::open(Path::new(&path)) {
        Ok(lookup) => {
            tracing::info!("city database loaded for GET /discovery/locate");
            Arc::new(lookup)
        },
        Err(err) => {
            tracing::warn!(
                error = %err,
                "{CITY_DB_ENV} could not be read - GET /discovery/locate answers {{}}"
            );
            Arc::new(NoCityLookup)
        },
    }
}

/// `GET /discovery/locate`. Answers `{city, country}`, part of it, or `{}`;
/// `Cache-Control: no-store` so no intermediary keeps an answer for an
/// address. Never an error: an address that cannot be identified or located
/// is `{}`.
pub(crate) async fn locate(State(state): State<AppState>, req: Request<Body>) -> Response {
    let place = crate::rate_limit::source_ip(&req)
        .and_then(|ip| state.city_lookup.locate(ip))
        .unwrap_or_default();
    ([(header::CACHE_CONTROL, "no-store")], Json(place)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_what_a_card_can_carry() {
        assert_eq!(
            sanitize(Some("Almere"), Some("nl")),
            Some(Place {
                city: Some("Almere".to_string()),
                country: Some("NL".to_string())
            })
        );
        assert_eq!(
            sanitize(None, Some("DE")),
            Some(Place {
                city: None,
                country: Some("DE".to_string())
            })
        );
        assert_eq!(sanitize(Some(""), Some("XYZ")), None);
        assert_eq!(sanitize(Some("A\nB"), None), None);
        assert_eq!(sanitize(Some(&"x".repeat(65)), None), None);
        assert_eq!(sanitize(None, None), None);
    }

    #[test]
    fn an_unreadable_database_is_an_error_not_a_panic() {
        assert!(MmdbCityLookup::open(Path::new("/nonexistent/lakko/absent.mmdb")).is_err());
        assert!(maxminddb::Reader::from_source(b"not a database".to_vec()).is_err());
        assert!(maxminddb::Reader::from_source(Vec::new()).is_err());
    }

    #[test]
    fn place_serializes_without_absent_halves() {
        assert_eq!(serde_json::to_string(&Place::default()).unwrap(), "{}");
        let place = Place {
            city: Some("Almere".to_string()),
            country: Some("NL".to_string()),
        };
        assert_eq!(
            serde_json::to_string(&place).unwrap(),
            r#"{"city":"Almere","country":"NL"}"#
        );
    }
}
