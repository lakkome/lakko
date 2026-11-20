// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0054 decision 1 - a group wake is a consequence of a signed publish.
//!
//! A member publishing to a claimed group journal may ask for the group's
//! devices to be woken by adding one header to the signed `POST`:
//!
//! ```text
//! x-lakko-wake: <sealed wake, base64url without padding>
//! ```
//!
//! Since ADR-0078 section 3 the value is a wake **sealed to push-gw's X25519
//! key** (`lakko_contacts::push_wake`): the `push_gid`, the kind, a nonce, a
//! timestamp, the group's unlock key and a signature by the group's wake
//! key are all inside. The DS checks the shape only - exactly
//! [`PUSH_WAKE_HEADER_LEN`] characters decoding to a [`PUSH_WAKE_LEN`]-byte
//! blob whose first byte is the wake version - and forwards the blob
//! unread. It no longer learns which group is woken, whether the wake is a
//! call, or anything else about it. No header, no wake: a committer beacon,
//! a typing signal or a receipt stays silent exactly as before.
//!
//! **Order.** The header is parsed before anything else is done with the
//! request, so a malformed one is a `400` and the row is never appended. The
//! wake is fired only after the ADR-0053 proof gate passed on a **claimed**
//! journal AND the row was appended. An unclaimed journal takes no proof, so
//! there is no writer to hold to the rule "only a writer may wake"; its
//! header is validated and dropped.
//!
//! **No binding.** ADR-0057 dropped a wake whose `push_gid` was not the one
//! bound to the publishing journal. The binding table, and the pepper it was
//! keyed under, are gone (the pair was the push-gw <-> journal join at rest):
//! push-gw now refuses a wake not signed by the group's wake key, which only
//! members hold, and deduplicates nonces. A member of two groups can still
//! carry one group's wake on the other group's journal; it could equally
//! publish to the first group, so nothing is gained by it.
//!
//! **Forward.** The DS hands `(wake, message_id)` to push-gw's internal
//! listener over HTTP, authenticated by a shared secret presented in
//! `x-lakko-internal-secret` (the transparency append-secret pattern). The
//! call is fire-and-forget: spawned, bounded by [`FORWARD_TIMEOUT`] and by
//! [`MAX_IN_FLIGHT`] concurrent forwards, and never able to delay or fail the
//! publish response. Nothing is stored, and no log line carries the blob or
//! a group id - at most the outcome.
//!
//! **Cap.** An in-memory token bucket per **journal** (the group id the
//! publish went to): a member who spams wakes through a journal is cut here.
//! The per-target cap that used to sit beside it keyed on a `push_gid` the DS
//! no longer sees; push-gw applies it after opening the wake. Over the cap
//! the message is appended and answered normally; only the wake is dropped.
//! A publish is never refused for its wake.
//!
//! The map is bounded ([`MAX_TRACKED`] keys): an entry idle for longer than a
//! full refill is indistinguishable from a fresh one and is swept first, then
//! the least recently used goes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, StatusCode};
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use base64::Engine as _;
use lakko_contacts::push_wake::{PUSH_WAKE_HEADER_LEN, PUSH_WAKE_LEN, PUSH_WAKE_VERSION_V1};
use tokio::sync::Semaphore;

use crate::journal::PSEUDONYM_LEN;
use crate::metrics::{self, WakeOutcome};

/// The request header a publisher asks for a wake with.
pub(crate) const WAKE_HEADER: &str = "x-lakko-wake";

/// The header push-gw's internal listener checks the shared secret in.
pub(crate) const INTERNAL_SECRET_HEADER: &str = "x-lakko-internal-secret";

/// Minimum decoded length of the shared secret; the transparency append
/// secret uses the same floor.
pub(crate) const MIN_SECRET_LEN: usize = 16;

/// Upper bound on one forward to push-gw. Loopback in production; a
/// push-gw that has not answered in this long is not going to.
pub(crate) const FORWARD_TIMEOUT: Duration = Duration::from_secs(5);

/// Most forwards in flight at once. A publish whose wake finds no free slot
/// drops the wake rather than queueing it behind a slow push-gw.
pub(crate) const MAX_IN_FLIGHT: usize = 256;

/// Most keys the bucket map tracks.
pub(crate) const MAX_TRACKED: usize = 65_536;

/// Default burst: wakes a journal may fire back to back.
///
/// Chosen, not measured. A busy human chat sends a message every few seconds
/// at its peak and a person typing quickly can send several in a row; 30
/// back-to-back wakes covers a flurry with room to spare, and a push provider
/// coalesces wakes for one device anyway.
pub(crate) const DEFAULT_BURST: u32 = 30;

/// Default sustained rate, wakes per minute per journal.
///
/// Chosen, not measured. One wake a second sustained is more than a group of
/// people typing by hand produces over any minute, and far below what a
/// script would need to drain batteries.
pub(crate) const DEFAULT_PER_MINUTE: u32 = 60;

type HandlerError = (StatusCode, String);

// ============================================================================
// The header
// ============================================================================

/// A parsed `x-lakko-wake` header: the sealed wake, unread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WakeRequest {
    /// The sealed wake bytes, exactly as the publisher sent them.
    pub(crate) blob: Vec<u8>,
}

fn bad_wake() -> HandlerError {
    (StatusCode::BAD_REQUEST, format!("malformed {WAKE_HEADER}"))
}

/// Parse the one header value. Strict: exactly [`PUSH_WAKE_HEADER_LEN`]
/// base64url-no-pad characters decoding to [`PUSH_WAKE_LEN`] bytes whose
/// first byte is the wake version. Nothing else is read.
pub(crate) fn parse_wake_value(value: &str) -> Result<WakeRequest, HandlerError> {
    if value.len() != PUSH_WAKE_HEADER_LEN {
        return Err(bad_wake());
    }
    // The no-pad engine refuses `=` and non-canonical trailing bits, so one
    // blob has exactly one spelling.
    let blob = B64URL.decode(value).map_err(|_| bad_wake())?;
    if blob.len() != PUSH_WAKE_LEN || blob[0] != PUSH_WAKE_VERSION_V1 {
        return Err(bad_wake());
    }
    Ok(WakeRequest { blob })
}

/// Read the wake header off a request. `Ok(None)` when absent; a `400` when
/// present but malformed, repeated, or not ASCII.
pub(crate) fn parse_wake_header(headers: &HeaderMap) -> Result<Option<WakeRequest>, HandlerError> {
    let mut values = headers.get_all(WAKE_HEADER).iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(bad_wake());
    }
    let value = first.to_str().map_err(|_| bad_wake())?;
    parse_wake_value(value).map(Some)
}

// ============================================================================
// Configuration
// ============================================================================

/// Burst and sustained rate of one bucket map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CapConfig {
    /// Tokens a full bucket holds.
    pub(crate) burst: u32,
    /// Tokens added per minute.
    pub(crate) per_minute: u32,
}

impl Default for CapConfig {
    fn default() -> Self {
        Self {
            burst: DEFAULT_BURST,
            per_minute: DEFAULT_PER_MINUTE,
        }
    }
}

/// Where wakes go, when they go anywhere.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ForwardConfig {
    /// push-gw's internal listener, e.g. `http://127.0.0.1:8092`. No
    /// trailing slash.
    pub(crate) base_url: String,
    /// The shared secret, base64 as configured; sent verbatim.
    pub(crate) secret_b64: String,
}

/// Everything [`Waker`] is built from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WakeConfig {
    /// `None`: dormant - headers are validated, wakes are dropped.
    pub(crate) forward: Option<ForwardConfig>,
    /// The per-journal cap.
    pub(crate) cap: CapConfig,
}

fn non_empty(lookup: &impl Fn(&str) -> Option<String>, name: &str) -> Option<String> {
    lookup(name).filter(|v| !v.trim().is_empty())
}

fn positive(
    lookup: &impl Fn(&str) -> Option<String>,
    name: &str,
    default: u32,
) -> Result<u32, String> {
    match non_empty(lookup, name) {
        None => Ok(default),
        Some(raw) => match raw.trim().parse::<u32>() {
            Ok(n) if n > 0 => Ok(n),
            _ => Err(format!("{name} must be a positive integer")),
        },
    }
}

impl WakeConfig {
    /// Read the configuration from the process environment.
    ///
    /// # Errors
    ///
    /// A message naming the variable when exactly one of
    /// `LAKKO_DS_PUSH_GW_URL` / `LAKKO_DS_PUSH_GW_SECRET` is set, when the
    /// URL is not `http(s)://`, when the secret is not base64 of at least
    /// [`MIN_SECRET_LEN`] bytes, or when a cap knob is not a positive integer.
    pub(crate) fn from_env() -> Result<Self, String> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// The testable core of [`Self::from_env`].
    pub(crate) fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let cap = CapConfig {
            burst: positive(&lookup, "LAKKO_DS_WAKE_BURST", DEFAULT_BURST)?,
            per_minute: positive(&lookup, "LAKKO_DS_WAKE_PER_MINUTE", DEFAULT_PER_MINUTE)?,
        };
        let url = non_empty(&lookup, "LAKKO_DS_PUSH_GW_URL");
        let secret = non_empty(&lookup, "LAKKO_DS_PUSH_GW_SECRET");
        let forward = match (url, secret) {
            (None, None) => None,
            (Some(_), None) => {
                return Err(
                    "LAKKO_DS_PUSH_GW_URL is set but LAKKO_DS_PUSH_GW_SECRET is not; \
                     set both to forward wakes, or neither to leave them off"
                        .to_string(),
                );
            },
            (None, Some(_)) => {
                return Err(
                    "LAKKO_DS_PUSH_GW_SECRET is set but LAKKO_DS_PUSH_GW_URL is not; \
                     set both to forward wakes, or neither to leave them off"
                        .to_string(),
                );
            },
            (Some(url), Some(secret)) => {
                let url = url.trim().trim_end_matches('/').to_string();
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    return Err("LAKKO_DS_PUSH_GW_URL must start with http:// or https://".into());
                }
                let secret = secret.trim().to_string();
                let decoded = B64
                    .decode(secret.as_bytes())
                    .map_err(|_| "LAKKO_DS_PUSH_GW_SECRET is not valid base64".to_string())?;
                if decoded.len() < MIN_SECRET_LEN {
                    return Err(format!(
                        "LAKKO_DS_PUSH_GW_SECRET decodes to {} bytes; need >= {MIN_SECRET_LEN}",
                        decoded.len()
                    ));
                }
                Some(ForwardConfig {
                    base_url: url,
                    secret_b64: secret,
                })
            },
        };
        Ok(Self { forward, cap })
    }
}

// ============================================================================
// Caps
// ============================================================================

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    last_seen: Instant,
}

/// A bounded map of token buckets keyed on a 32-byte id.
pub(crate) struct WakeCap {
    buckets: Mutex<HashMap<[u8; PSEUDONYM_LEN], Bucket>>,
    rate_per_sec: f64,
    burst: f64,
    /// Time for an empty bucket to refill completely.
    idle_after: Duration,
    max_entries: usize,
}

impl WakeCap {
    /// A cap with `config`, tracking at most `max_entries` keys.
    pub(crate) fn new(config: CapConfig, max_entries: usize) -> Self {
        // `WakeConfig` guarantees both values are >= 1; the `max` keeps a
        // hand-built config from dividing by zero.
        let rate_per_sec = f64::from(config.per_minute.max(1)) / 60.0;
        let burst = f64::from(config.burst.max(1));
        let idle_after =
            Duration::try_from_secs_f64(burst / rate_per_sec).unwrap_or(Duration::from_secs(60));
        Self {
            buckets: Mutex::new(HashMap::new()),
            rate_per_sec,
            burst,
            idle_after,
            max_entries: max_entries.max(1),
        }
    }

    /// Take one token for `key` at `now`; `true` when there was one.
    pub(crate) fn admit_at(&self, key: &[u8; PSEUDONYM_LEN], now: Instant) -> bool {
        let mut buckets = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        if !buckets.contains_key(key) && buckets.len() >= self.max_entries {
            self.evict(&mut buckets, now);
        }
        let bucket = buckets.entry(*key).or_insert(Bucket {
            tokens: self.burst,
            last_seen: now,
        });
        let elapsed = now
            .saturating_duration_since(bucket.last_seen)
            .as_secs_f64();
        bucket.tokens = elapsed
            .mul_add(self.rate_per_sec, bucket.tokens)
            .min(self.burst);
        bucket.last_seen = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn evict(&self, buckets: &mut HashMap<[u8; PSEUDONYM_LEN], Bucket>, now: Instant) {
        buckets.retain(|_, b| now.saturating_duration_since(b.last_seen) < self.idle_after);
        if buckets.len() >= self.max_entries {
            if let Some(oldest) = buckets
                .iter()
                .min_by_key(|(_, b)| b.last_seen)
                .map(|(k, _)| *k)
            {
                buckets.remove(&oldest);
            }
        }
    }

    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.buckets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

// ============================================================================
// The waker
// ============================================================================

struct Forwarder {
    client: reqwest::Client,
    notify_url: String,
    secret_b64: String,
    slots: Arc<Semaphore>,
}

/// Holds the cap and, when configured, the forwarder to push-gw.
pub(crate) struct Waker {
    forward: Option<Forwarder>,
    per_journal: WakeCap,
    /// Every blob handed to [`Self::fire`], so a route test can tell a fired
    /// wake from a dropped one without a push-gw.
    #[cfg(test)]
    fired: Mutex<Vec<Vec<u8>>>,
}

impl Waker {
    /// Build from `config`.
    ///
    /// # Errors
    ///
    /// When the HTTP client cannot be built.
    pub(crate) fn new(config: &WakeConfig) -> Result<Self, String> {
        let forward = match &config.forward {
            None => None,
            Some(f) => {
                let client = reqwest::Client::builder()
                    .timeout(FORWARD_TIMEOUT)
                    .connect_timeout(FORWARD_TIMEOUT)
                    // The internal listener is at a fixed address; a redirect
                    // would carry the secret somewhere else.
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(|e| format!("build push-gw client: {e}"))?;
                Some(Forwarder {
                    client,
                    notify_url: format!("{}/push/notify-group", f.base_url),
                    secret_b64: f.secret_b64.clone(),
                    slots: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
                })
            },
        };
        Ok(Self {
            forward,
            per_journal: WakeCap::new(config.cap, MAX_TRACKED),
            #[cfg(test)]
            fired: Mutex::new(Vec::new()),
        })
    }

    /// A waker that forwards nothing, with the default cap.
    #[cfg(test)]
    pub(crate) fn dormant() -> Self {
        Self {
            forward: None,
            per_journal: WakeCap::new(CapConfig::default(), MAX_TRACKED),
            fired: Mutex::new(Vec::new()),
        }
    }

    /// The blobs handed to [`Self::fire`] so far.
    #[cfg(test)]
    pub(crate) fn fired_targets(&self) -> Vec<Vec<u8>> {
        self.fired
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// `true` when wakes are forwarded.
    pub(crate) const fn is_active(&self) -> bool {
        self.forward.is_some()
    }

    /// Charge the journal's cap for one wake; `true` when it had a token.
    pub(crate) fn admit_at(&self, journal: &[u8; PSEUDONYM_LEN], now: Instant) -> bool {
        self.per_journal.admit_at(journal, now)
    }

    /// Fire a wake for the row `message_id` just appended to `journal`.
    /// Returns at once; the forward (if any) runs on its own task.
    pub(crate) fn fire(&self, journal: &[u8; PSEUDONYM_LEN], wake: WakeRequest, message_id: u64) {
        #[cfg(test)]
        self.fired
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(wake.blob.clone());
        if !self.admit_at(journal, Instant::now()) {
            metrics::wake(WakeOutcome::Capped);
            tracing::debug!(outcome = "journal-capped", "wake dropped");
            return;
        }
        let Some(forward) = &self.forward else {
            metrics::wake(WakeOutcome::Dormant);
            tracing::debug!(outcome = "dormant", "wake dropped");
            return;
        };
        let Ok(permit) = Arc::clone(&forward.slots).try_acquire_owned() else {
            metrics::wake(WakeOutcome::Busy);
            tracing::warn!(
                outcome = "busy",
                "wake dropped: forwards in flight at limit"
            );
            return;
        };
        let request = forward
            .client
            .post(&forward.notify_url)
            .header(INTERNAL_SECRET_HEADER, &forward.secret_b64)
            .json(&serde_json::json!({
                "wake": B64URL.encode(&wake.blob),
                "envelope_pointer": message_id,
            }));
        tokio::spawn(async move {
            let _permit = permit;
            match request.send().await {
                Ok(resp) if resp.status().is_success() => {
                    metrics::wake(WakeOutcome::Forwarded);
                    tracing::debug!(outcome = "forwarded", "wake forwarded");
                },
                Ok(resp) => {
                    metrics::wake(WakeOutcome::Refused);
                    // push-gw refuses a wake it cannot open, an unsigned or
                    // replayed one, or one over its per-group cap.
                    tracing::debug!(status = resp.status().as_u16(), "wake refused by push-gw");
                },
                Err(e) => {
                    metrics::wake(WakeOutcome::Failed);
                    // The error's own text can carry the URL, never the blob
                    // (it rides in the body).
                    tracing::warn!(timeout = e.is_timeout(), "wake forward failed");
                },
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn blob(fill: u8) -> Vec<u8> {
        let mut b = vec![fill; PUSH_WAKE_LEN];
        b[0] = PUSH_WAKE_VERSION_V1;
        b
    }

    fn headers_with(values: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in values {
            h.append(WAKE_HEADER, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn a_sealed_wake_parses_to_its_bytes() {
        let w = parse_wake_value(&B64URL.encode(blob(7))).unwrap();
        assert_eq!(w.blob, blob(7));
    }

    #[test]
    fn an_absent_header_is_no_wake() {
        assert_eq!(parse_wake_header(&HeaderMap::new()).unwrap(), None);
    }

    #[test]
    fn a_present_header_is_read() {
        let h = headers_with(&[&B64URL.encode(blob(1))]);
        assert_eq!(parse_wake_header(&h).unwrap().unwrap().blob, blob(1));
    }

    #[test]
    fn every_malformed_class_is_a_400() {
        let good = B64URL.encode(blob(9));
        let mut wrong_version = blob(9);
        wrong_version[0] = 0x02;
        // 0xfb bytes produce '+' and '/' in the standard alphabet.
        let standard = B64.encode(blob(0xfb)).trim_end_matches('=').to_string();
        let cases = vec![
            String::new(),
            good[..good.len() - 1].to_string(),
            format!("{good}A"),
            B64URL.encode(&wrong_version),
            format!("{}=", &good[..good.len() - 1]),
            standard,
            "!".repeat(PUSH_WAKE_HEADER_LEN),
            format!("{};message", B64URL.encode([1u8; 32])),
            format!(" {}", &good[1..]),
        ];
        for case in cases {
            let err = parse_wake_value(&case).unwrap_err();
            assert_eq!(err.0, StatusCode::BAD_REQUEST, "accepted {case:?}");
        }
    }

    #[test]
    fn a_repeated_header_is_a_400() {
        let v = B64URL.encode(blob(1));
        let h = headers_with(&[&v, &v]);
        assert_eq!(
            parse_wake_header(&h).unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn a_non_ascii_header_is_a_400() {
        let mut h = HeaderMap::new();
        h.insert(WAKE_HEADER, HeaderValue::from_bytes(&[0xe9, b';']).unwrap());
        assert_eq!(
            parse_wake_header(&h).unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
    }

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_string())
        }
    }

    const SECRET: &str = "c2hhcmVkLXdha2Utc2VjcmV0LWZvci10ZXN0cy0zMmI="; // 32 bytes

    #[test]
    fn neither_variable_is_dormant_with_default_caps() {
        let cfg = WakeConfig::from_lookup(env(&[])).unwrap();
        assert_eq!(cfg.forward, None);
        assert_eq!(cfg.cap, CapConfig::default());
        assert!(!Waker::new(&cfg).unwrap().is_active());
    }

    #[test]
    fn empty_values_count_as_unset() {
        let cfg = WakeConfig::from_lookup(env(&[
            ("LAKKO_DS_PUSH_GW_URL", ""),
            ("LAKKO_DS_PUSH_GW_SECRET", "  "),
        ]))
        .unwrap();
        assert_eq!(cfg.forward, None);
    }

    #[test]
    fn only_one_variable_refuses_to_start() {
        assert!(
            WakeConfig::from_lookup(env(&[("LAKKO_DS_PUSH_GW_URL", "http://127.0.0.1:8092")]))
                .unwrap_err()
                .contains("LAKKO_DS_PUSH_GW_SECRET")
        );
        assert!(
            WakeConfig::from_lookup(env(&[("LAKKO_DS_PUSH_GW_SECRET", SECRET)]))
                .unwrap_err()
                .contains("LAKKO_DS_PUSH_GW_URL")
        );
    }

    #[test]
    fn both_variables_configure_the_forward() {
        let cfg = WakeConfig::from_lookup(env(&[
            ("LAKKO_DS_PUSH_GW_URL", "http://127.0.0.1:8092/"),
            ("LAKKO_DS_PUSH_GW_SECRET", SECRET),
        ]))
        .unwrap();
        let f = cfg.forward.clone().unwrap();
        assert_eq!(f.base_url, "http://127.0.0.1:8092");
        assert_eq!(f.secret_b64, SECRET);
        assert!(Waker::new(&cfg).unwrap().is_active());
    }

    #[test]
    fn a_bad_secret_or_url_refuses_to_start() {
        let short = B64.encode([1u8; 8]);
        for (url, secret) in [
            ("http://127.0.0.1:8092", "not base64 !!"),
            ("http://127.0.0.1:8092", short.as_str()),
            ("127.0.0.1:8092", SECRET),
        ] {
            assert!(
                WakeConfig::from_lookup(env(&[
                    ("LAKKO_DS_PUSH_GW_URL", url),
                    ("LAKKO_DS_PUSH_GW_SECRET", secret),
                ]))
                .is_err(),
                "accepted {url} / {secret}"
            );
        }
    }

    #[test]
    fn cap_knobs_are_read_and_must_be_positive() {
        let cfg = WakeConfig::from_lookup(env(&[
            ("LAKKO_DS_WAKE_BURST", "5"),
            ("LAKKO_DS_WAKE_PER_MINUTE", "12"),
        ]))
        .unwrap();
        assert_eq!(
            cfg.cap,
            CapConfig {
                burst: 5,
                per_minute: 12
            }
        );
        for bad in ["0", "-1", "x"] {
            assert!(WakeConfig::from_lookup(env(&[("LAKKO_DS_WAKE_BURST", bad)])).is_err());
            assert!(WakeConfig::from_lookup(env(&[("LAKKO_DS_WAKE_PER_MINUTE", bad)])).is_err());
        }
    }

    fn waker(burst: u32, per_minute: u32) -> Waker {
        Waker::new(&WakeConfig {
            forward: None,
            cap: CapConfig { burst, per_minute },
        })
        .unwrap()
    }

    #[test]
    fn a_journal_is_capped_after_its_burst_and_refills() {
        // 60/min = one token a second.
        let w = waker(3, 60);
        let t0 = Instant::now();
        let journal = [1u8; PSEUDONYM_LEN];
        for _ in 0..3 {
            assert!(w.admit_at(&journal, t0));
        }
        assert!(!w.admit_at(&journal, t0));
        assert!(w.admit_at(&journal, t0 + Duration::from_millis(1100)));
        // Another journal has its own budget.
        assert!(w.admit_at(&[2u8; PSEUDONYM_LEN], t0));
    }

    #[test]
    fn the_cap_maps_never_exceed_their_ceiling() {
        let cap = WakeCap::new(
            CapConfig {
                burst: 5,
                per_minute: 60,
            },
            4,
        );
        let t0 = Instant::now();
        for n in 0..50u8 {
            cap.admit_at(&[n; 32], t0 + Duration::from_millis(u64::from(n)));
            assert!(
                cap.tracked() <= 4,
                "tracked {} after key {n}",
                cap.tracked()
            );
        }
    }

    #[test]
    fn idle_entries_are_evicted_first() {
        // burst 5 at 60/min refills fully in 5 s.
        let cap = WakeCap::new(
            CapConfig {
                burst: 5,
                per_minute: 60,
            },
            2,
        );
        let t0 = Instant::now();
        cap.admit_at(&[1; 32], t0);
        cap.admit_at(&[2; 32], t0 + Duration::from_secs(9));
        cap.admit_at(&[3; 32], t0 + Duration::from_secs(10));
        let buckets = cap.buckets.lock().unwrap();
        assert!(!buckets.contains_key(&[1; 32]));
        assert!(buckets.contains_key(&[2; 32]));
        assert!(buckets.contains_key(&[3; 32]));
    }

    #[tokio::test]
    async fn an_admitted_wake_reaches_push_gw_with_the_secret() {
        use axum::extract::State;
        use axum::routing::post;
        use axum::Router;
        use tokio::sync::mpsc;

        type Seen = (Option<String>, serde_json::Value);
        let (tx, mut rx) = mpsc::unbounded_channel::<Seen>();
        let stub = Router::new()
            .route(
                "/push/notify-group",
                post(
                    |State(tx): State<mpsc::UnboundedSender<Seen>>,
                     headers: HeaderMap,
                     axum::Json(body): axum::Json<serde_json::Value>| async move {
                        let secret = headers
                            .get(INTERNAL_SECRET_HEADER)
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string);
                        let _ = tx.send((secret, body));
                        StatusCode::OK
                    },
                ),
            )
            .with_state(tx);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });

        let w = Waker::new(&WakeConfig {
            forward: Some(ForwardConfig {
                base_url: format!("http://{addr}"),
                secret_b64: SECRET.to_string(),
            }),
            cap: CapConfig::default(),
        })
        .unwrap();
        w.fire(&[1; 32], WakeRequest { blob: blob(0xab) }, 77);

        let (secret, body) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("forward within 5 s")
            .expect("stub saw the request");
        assert_eq!(secret.as_deref(), Some(SECRET));
        // The blob byte for byte, unread, beside the message id.
        assert_eq!(body["wake"], B64URL.encode(blob(0xab)));
        assert_eq!(body["envelope_pointer"], 77);
        assert_eq!(
            body.as_object().unwrap().len(),
            2,
            "nothing else rides along"
        );
    }

    #[tokio::test]
    async fn an_unreachable_push_gw_never_blocks_the_caller() {
        // Port 9 (discard) on loopback is closed on any test host; the forward
        // fails on its own task and `fire` has already returned.
        let w = Waker::new(&WakeConfig {
            forward: Some(ForwardConfig {
                base_url: "http://127.0.0.1:9".to_string(),
                secret_b64: SECRET.to_string(),
            }),
            cap: CapConfig::default(),
        })
        .unwrap();
        let started = Instant::now();
        w.fire(&[1; 32], WakeRequest { blob: blob(1) }, 1);
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[tokio::test]
    async fn a_dormant_waker_drops_the_wake_without_a_task() {
        // Nothing to assert beyond "does not panic and returns at once":
        // there is no forwarder to reach.
        let w = Waker::dormant();
        w.fire(&[1; 32], WakeRequest { blob: blob(1) }, 42);
    }
}
