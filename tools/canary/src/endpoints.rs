// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Where the canary sends its requests.
//!
//! A request has two paths: the **service path** the service routes on and
//! every proof (DS, storage) is signed over, and the URL it is sent to, which
//! is a base (possibly carrying a reverse-proxy prefix such as `/api/ds`)
//! followed by that service path. Keeping the two apart is what lets one
//! signing call serve both a public deployment behind nginx and a local
//! stack where each service listens on its own port.

use serde::{Deserialize, Serialize};

use crate::error::Error;

/// The service bases, as written into the state file at `init`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Endpoints {
    /// Base of the auth service, e.g. `https://lakko.me/api/auth`.
    pub(crate) auth: String,
    /// Base of the Delivery Service, e.g. `https://lakko.me/api/ds`.
    pub(crate) ds: String,
    /// Base of the storage service, e.g. `https://lakko.me/api/storage`.
    pub(crate) storage: String,
    /// Base of the SFU signaling endpoint, `ws://` or `wss://`, e.g.
    /// `wss://sfu.lakko.me` or `wss://test.lakko.me/sfu`.
    pub(crate) sfu: String,
}

impl Endpoints {
    /// Build from the public base and the per-service overrides.
    ///
    /// `api` is the public base (`https://lakko.me`); the services sit under
    /// `/api/auth`, `/api/ds` and `/api/storage` there (nginx strips the
    /// prefix). An override replaces one service's base entirely.
    ///
    /// # Errors
    ///
    /// [`Error::Usage`] when a base is missing or not `http(s)` / the SFU
    /// base is not `ws(s)`.
    pub(crate) fn build(
        api: Option<&str>,
        auth: Option<&str>,
        ds: Option<&str>,
        storage: Option<&str>,
        sfu: &str,
    ) -> Result<Self, Error> {
        let api = api.map(trim);
        let pick = |over: Option<&str>, prefix: &str| -> Result<String, Error> {
            match (over, api) {
                (Some(o), _) => Ok(trim(o).to_owned()),
                (None, Some(a)) => Ok(format!("{a}{prefix}")),
                (None, None) => Err(Error::Usage(
                    "--api or every per-service base is required".to_owned(),
                )),
            }
        };
        let out = Self {
            auth: pick(auth, "/api/auth")?,
            ds: pick(ds, "/api/ds")?,
            storage: pick(storage, "/api/storage")?,
            sfu: trim(sfu).to_owned(),
        };
        out.check()?;
        Ok(out)
    }

    /// Every base is `https` / `wss`, or plain `http` / `ws` to a loopback
    /// host only: a plain scheme anywhere else would send session tokens,
    /// anonymous tokens, storage passes and call JWTs in the clear.
    ///
    /// # Errors
    ///
    /// [`Error::Usage`] naming the first base that fails.
    pub(crate) fn check(&self) -> Result<(), Error> {
        for base in [&self.auth, &self.ds, &self.storage] {
            if !scheme_allowed(base, "https://", "http://") {
                return Err(Error::Usage(
                    "service bases must be https URLs (http only to a loopback host)".to_owned(),
                ));
            }
        }
        if !scheme_allowed(&self.sfu, "wss://", "ws://") {
            return Err(Error::Usage(
                "--sfu must be a wss URL (ws only to a loopback host)".to_owned(),
            ));
        }
        Ok(())
    }

    /// URL of an auth route.
    #[must_use]
    pub(crate) fn auth_url(&self, service_path: &str) -> String {
        join(&self.auth, service_path, "")
    }

    /// URL of a DS route with an optional raw query (without `?`).
    #[must_use]
    pub(crate) fn ds_url(&self, service_path: &str, query: &str) -> String {
        join(&self.ds, service_path, query)
    }

    /// URL of a storage route.
    #[must_use]
    pub(crate) fn storage_url(&self, service_path: &str) -> String {
        join(&self.storage, service_path, "")
    }

    /// The SFU signaling URL: the SFU base followed by `/v1/connect`.
    #[must_use]
    pub(crate) fn sfu_connect_url(&self) -> String {
        join(&self.sfu, "/v1/connect", "")
    }
}

fn trim(s: &str) -> &str {
    s.trim().trim_end_matches('/')
}

fn join(base: &str, service_path: &str, query: &str) -> String {
    if query.is_empty() {
        format!("{base}{service_path}")
    } else {
        format!("{base}{service_path}?{query}")
    }
}

/// The DS service path of a group journal route (`/ds/group/<hex>/<tail>`).
#[must_use]
pub(crate) fn ds_group_path(group_id: &[u8], tail: &str) -> String {
    format!("/ds/group/{}/{tail}", hex::encode(group_id))
}

/// The storage service path of a blob.
#[must_use]
pub(crate) fn blob_path(blob_id: &[u8; 32]) -> String {
    format!("/blob/{}", hex::encode(blob_id))
}

/// `secure` always; `plain` only when the URL's host is loopback.
fn scheme_allowed(url: &str, secure: &str, plain: &str) -> bool {
    if url.starts_with(secure) {
        return url.len() > secure.len();
    }
    url.strip_prefix(plain).is_some_and(is_loopback_authority)
}

/// Whether the authority at the start of `rest` (`host[:port][/...]`) names
/// a loopback host: `localhost`, an IPv4 address in 127.0.0.0/8, or `[::1]`.
fn is_loopback_authority(rest: &str) -> bool {
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return false;
    }
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        match bracketed.split_once(']') {
            Some((inner, _)) => inner,
            None => return false,
        }
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_base_gets_the_nginx_prefixes_and_signed_path_stays_bare() {
        let e = Endpoints::build(
            Some("https://lakko.me/"),
            None,
            None,
            None,
            "wss://sfu.lakko.me",
        )
        .unwrap();
        let gid = [0xAB; 32];
        let path = ds_group_path(&gid, "messages");
        assert_eq!(path, format!("/ds/group/{}/messages", "ab".repeat(32)));
        assert_eq!(
            e.ds_url(&path, "after=3&limit=100&wait=25"),
            format!("https://lakko.me/api/ds{path}?after=3&limit=100&wait=25")
        );
        assert!(!path.contains("/api/"));
        assert_eq!(
            e.auth_url("/login/start"),
            "https://lakko.me/api/auth/login/start"
        );
        let blob = blob_path(&[1; 32]);
        assert_eq!(
            e.storage_url(&blob),
            format!("https://lakko.me/api/storage{blob}")
        );
        assert_eq!(e.sfu_connect_url(), "wss://sfu.lakko.me/v1/connect");
    }

    #[test]
    fn sfu_base_with_a_path_prefix_keeps_it() {
        let e = Endpoints::build(
            Some("https://test.lakko.me"),
            None,
            None,
            None,
            "wss://test.lakko.me/sfu/",
        )
        .unwrap();
        assert_eq!(e.sfu_connect_url(), "wss://test.lakko.me/sfu/v1/connect");
    }

    #[test]
    fn per_service_overrides_replace_the_base() {
        let e = Endpoints::build(
            None,
            Some("http://127.0.0.1:1"),
            Some("http://127.0.0.1:2/"),
            Some("http://127.0.0.1:3"),
            "ws://127.0.0.1:4",
        )
        .unwrap();
        assert_eq!(e.ds_url("/ds/x", ""), "http://127.0.0.1:2/ds/x");
        assert_eq!(e.auth, "http://127.0.0.1:1");
    }

    #[test]
    fn missing_or_wrong_scheme_is_refused() {
        assert!(Endpoints::build(None, None, None, None, "wss://x").is_err());
        assert!(Endpoints::build(Some("ftp://x"), None, None, None, "wss://x").is_err());
        assert!(Endpoints::build(Some("https://x"), None, None, None, "https://x").is_err());
    }

    #[test]
    fn plain_schemes_only_to_loopback() {
        for ok in [
            "http://127.0.0.1:8081",
            "http://localhost/x",
            "http://[::1]:1",
            "http://127.9.9.9",
        ] {
            assert!(
                Endpoints::build(Some(ok), None, None, None, "wss://sfu.example").is_ok(),
                "{ok}"
            );
        }
        for bad in [
            "http://lakko.me",
            "http://10.0.0.1:8081",
            "http://127.0.0.1.example.com",
            "http://user@127.0.0.1",
            "http://[::1",
            "http://",
        ] {
            assert!(
                Endpoints::build(Some(bad), None, None, None, "wss://sfu.example").is_err(),
                "{bad}"
            );
        }
        assert!(
            Endpoints::build(Some("https://a"), None, None, None, "ws://127.0.0.1:7880").is_ok()
        );
        assert!(
            Endpoints::build(Some("https://a"), None, None, None, "ws://sfu.lakko.me").is_err()
        );
        assert!(Endpoints::build(Some("https://a"), None, None, None, "wss://").is_err());
    }
}
