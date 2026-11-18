// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Signed donation methods (`docs/ux/donations.md` section 3).
//!
//! The donations page shows payment links, a QR image, a phone number and
//! crypto addresses that `services/discovery` serves. Whoever controls that
//! server could replace an address with their own, so every method is
//! signed **offline** by a dedicated donations key that is never on a
//! server, and the client shows only what verifies against the public key
//! pinned into its build. The server keeps one power: choosing which signed
//! methods a request sees (the country rule stays unsigned and server-side).
//!
//! Two kinds, both fixed-field, length-prefixed, bounded and fail-closed,
//! each with its own domain separator so neither verifies as the other, nor
//! as any other kind in this crate:
//!
//! ```text
//! method statement (DOMAIN_METHOD = b"lakko/donation-method/v1")
//!   version(1)=0x02 ‖ signed_at_unix_millis(8 BE i64)
//!   ‖ id_len(1) ‖ id ‖ kind(1: 1 sbp, 2 revolut, 3 bmc, 4 crypto)
//!   ‖ icon(1: 0 none, 1 btc, 2 eth, 3 ton)
//!   ‖ link_len(2 BE) ‖ link ‖ coin_len(1) ‖ coin ‖ network_len(1) ‖ network
//!   ‖ address_len(2 BE) ‖ address
//!   ‖ qr_sha256_flag(1: 0|1) ‖ [qr_sha256(32)] ‖ has_phone(1: 0|1)
//!   ‖ Ed25519_sig(64)
//!
//! phone statement (DOMAIN_PHONE = b"lakko/donation-phone/v1")
//!   version(1)=0x01 ‖ id_len(1) ‖ id ‖ phone_len(1) ‖ phone ‖ Ed25519_sig(64)
//!
//! signing input = DOMAIN ‖ every byte before the signature
//! ```
//!
//! A method statement of version 1 (no `icon` byte) is refused: there was
//! never a deployment to stay compatible with. The phone statement did not
//! change and stays version 1.
//!
//! An absent optional text field is a zero length; a present one is never
//! empty, so the encoding is canonical (one method has one wire form).
//!
//! Field rules, checked on both sign and open:
//!
//! - `id`: 1..=32 bytes of `[a-z0-9_-]`;
//! - `link` and `address`: at most 512 bytes, no white space and no control
//!   character; a link starts with `https://`;
//! - `coin` and `network`: at most 64 bytes, no control character;
//! - `phone`: 1..=32 bytes of `+0-9 ()-`, at least one digit.
//! - a crypto method carries coin, network and address and nothing else (no
//!   link, no QR — the client draws the address's QR — no phone); its `icon`
//!   names the coin mark drawn in the centre of that QR (`none` = a plain QR);
//! - any other kind carries no coin, network or address, `icon` none, and at
//!   least one of a link, a QR and a phone (a card with none of them has
//!   nothing to show; its QR is the operator's own image).
//! - `icon` is a closed list; an unknown byte is refused.
//!
//! `qr_sha256` is the SHA-256 of the QR PNG the server serves for the method,
//! so a client checks the image it fetched against the signed hash.
//!
//! The phone number is not in the method statement (the list route must not
//! carry it, and a hash of a phone number is easily reversed); the method
//! carries `has_phone`, and the phone statement names the method id it
//! belongs to, which the verifier checks.
//!
//! What the signature does not stop: the server can hide methods, and it can
//! serve an older statement signed under the same key for the same id (a
//! retired address or phone). There is no expiry; `signed_at` lets a client
//! compare. See `docs/security/known-gaps.md`.

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, SIGNATURE_LEN};
use thiserror::Error;

/// Domain separator of a method statement.
const DOMAIN_METHOD: &[u8] = b"lakko/donation-method/v1";

/// Domain separator of a phone statement.
const DOMAIN_PHONE: &[u8] = b"lakko/donation-phone/v1";

/// Wire-format version of a method statement. Any other value is refused.
const METHOD_VERSION: u8 = 2;

/// Wire-format version of a phone statement. Any other value is refused.
const PHONE_VERSION: u8 = 1;

/// Longest method id, in bytes.
pub const DONATION_ID_MAX: usize = 32;
/// Longest link or address, in bytes.
pub const DONATION_LINK_MAX: usize = 512;
/// Longest coin or network name, in bytes.
pub const DONATION_COIN_MAX: usize = 64;
/// Longest phone number, in bytes.
pub const DONATION_PHONE_MAX: usize = 32;
/// Length of the QR image hash.
pub const DONATION_QR_HASH_LEN: usize = 32;

/// Longest method statement, in bytes.
pub const DONATION_METHOD_MAX_LEN: usize = 1
    + 8
    + 1
    + DONATION_ID_MAX
    + 1
    + 1
    + 2
    + DONATION_LINK_MAX
    + 1
    + DONATION_COIN_MAX
    + 1
    + DONATION_COIN_MAX
    + 2
    + DONATION_LINK_MAX
    + 1
    + DONATION_QR_HASH_LEN
    + 1
    + SIGNATURE_LEN;

/// Longest phone statement, in bytes.
pub const DONATION_PHONE_MAX_LEN: usize =
    1 + 1 + DONATION_ID_MAX + 1 + DONATION_PHONE_MAX + SIGNATURE_LEN;

const _: () = assert!(DONATION_METHOD_MAX_LEN == 1300);
const _: () = assert!(DONATION_PHONE_MAX_LEN == 131);

/// Why a donation statement was refused or could not be signed.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DonationError {
    /// The bytes end before a length prefix says, run past the end, or are
    /// longer than any statement can be.
    #[error("donation statement has the wrong length")]
    BadLength,
    /// The version byte is not the statement's version.
    #[error("unsupported donation statement version: {0}")]
    BadVersion(u8),
    /// The kind byte is not one of the four kinds.
    #[error("unknown donation kind: {0}")]
    BadKind(u8),
    /// A field breaks its bound or character rule (or a flag is not 0 or 1).
    #[error("donation field {0} is out of bounds")]
    BadField(&'static str),
    /// The fields present do not fit the method's kind.
    #[error("donation fields do not fit the kind")]
    KindMismatch,
    /// The Ed25519 signature did not verify, or the key is not a key.
    #[error("donation statement signature is invalid")]
    BadSignature,
    /// A phone statement names another method.
    #[error("phone statement belongs to another method")]
    IdMismatch,
}

impl DonationError {
    /// A stable kebab-case code for the FFI and for logs that must not carry
    /// the statement's content.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::BadLength => "bad-length",
            Self::BadVersion(_) => "bad-version",
            Self::BadKind(_) => "bad-kind",
            Self::BadField(_) => "bad-field",
            Self::KindMismatch => "kind-mismatch",
            Self::BadSignature => "bad-signature",
            Self::IdMismatch => "id-mismatch",
        }
    }
}

/// The kind of a donation method; the client picks its description line from
/// it, so the set is closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DonationKind {
    /// SBP, a Russian bank transfer (wire 1).
    Sbp,
    /// Revolut (wire 2).
    Revolut,
    /// Buy Me a Coffee (wire 3).
    Bmc,
    /// A crypto wallet (wire 4).
    Crypto,
}

impl DonationKind {
    /// The wire byte.
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        match self {
            Self::Sbp => 1,
            Self::Revolut => 2,
            Self::Bmc => 3,
            Self::Crypto => 4,
        }
    }

    /// Parse the wire byte.
    ///
    /// # Errors
    ///
    /// [`DonationError::BadKind`] for any other byte.
    pub const fn from_byte(b: u8) -> Result<Self, DonationError> {
        match b {
            1 => Ok(Self::Sbp),
            2 => Ok(Self::Revolut),
            3 => Ok(Self::Bmc),
            4 => Ok(Self::Crypto),
            other => Err(DonationError::BadKind(other)),
        }
    }

    /// The lower-case name (`sbp`, `revolut`, `bmc`, `crypto`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sbp => "sbp",
            Self::Revolut => "revolut",
            Self::Bmc => "bmc",
            Self::Crypto => "crypto",
        }
    }

    /// Parse the lower-case name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "sbp" => Some(Self::Sbp),
            "revolut" => Some(Self::Revolut),
            "bmc" => Some(Self::Bmc),
            "crypto" => Some(Self::Crypto),
            _ => None,
        }
    }
}

/// The coin mark a client draws in the centre of a crypto method's QR. A
/// closed list: the client carries the artwork, so a new mark is a new
/// release.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DonationIcon {
    /// No mark: a plain QR, and every payment method (wire 0).
    #[default]
    None,
    /// The Bitcoin mark (wire 1).
    Btc,
    /// The Ethereum mark (wire 2).
    Eth,
    /// The TON mark (wire 3).
    Ton,
}

impl DonationIcon {
    /// The wire byte.
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Btc => 1,
            Self::Eth => 2,
            Self::Ton => 3,
        }
    }

    /// Parse the wire byte.
    ///
    /// # Errors
    ///
    /// [`DonationError::BadField`] (`icon`) for any other byte.
    pub const fn from_byte(b: u8) -> Result<Self, DonationError> {
        match b {
            0 => Ok(Self::None),
            1 => Ok(Self::Btc),
            2 => Ok(Self::Eth),
            3 => Ok(Self::Ton),
            _ => Err(DonationError::BadField("icon")),
        }
    }

    /// The lower-case name (`none`, `btc`, `eth`, `ton`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Btc => "btc",
            Self::Eth => "eth",
            Self::Ton => "ton",
        }
    }

    /// Parse the lower-case name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "none" => Some(Self::None),
            "btc" => Some(Self::Btc),
            "eth" => Some(Self::Eth),
            "ton" => Some(Self::Ton),
            _ => None,
        }
    }
}

/// The content of a method statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DonationMethod {
    /// When the statement was signed, Unix milliseconds.
    pub signed_at_unix_millis: i64,
    /// The stable short id (`sbp`, `btc`, ...).
    pub id: String,
    /// The kind.
    pub kind: DonationKind,
    /// The coin mark in the centre of a crypto method's QR; always
    /// [`DonationIcon::None`] for a payment method.
    pub icon: DonationIcon,
    /// The payment link, `https://` only.
    pub link: Option<String>,
    /// The coin of a crypto method.
    pub coin: Option<String>,
    /// The network of a crypto method.
    pub network: Option<String>,
    /// The address of a crypto method.
    pub address: Option<String>,
    /// SHA-256 of the QR PNG the server serves for this method.
    pub qr_sha256: Option<[u8; DONATION_QR_HASH_LEN]>,
    /// Whether a phone statement exists for this method.
    pub has_phone: bool,
}

/// The content of a phone statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DonationPhone {
    /// The method id the number belongs to.
    pub id: String,
    /// The phone number as shown.
    pub phone: String,
}

fn check_id(id: &str) -> Result<(), DonationError> {
    let ok = !id.is_empty()
        && id.len() <= DONATION_ID_MAX
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(DonationError::BadField("id"))
    }
}

/// A present text field: non-empty, within `max` bytes, no control
/// character, and (when `no_space`) no white space.
fn check_text(
    value: Option<&str>,
    max: usize,
    no_space: bool,
    what: &'static str,
) -> Result<(), DonationError> {
    let Some(value) = value else { return Ok(()) };
    let ok = !value.is_empty()
        && value.len() <= max
        && !value
            .chars()
            .any(|c| c.is_control() || (no_space && c.is_whitespace()));
    if ok {
        Ok(())
    } else {
        Err(DonationError::BadField(what))
    }
}

fn check_phone(phone: &str) -> Result<(), DonationError> {
    let ok = !phone.is_empty()
        && phone.len() <= DONATION_PHONE_MAX
        && phone
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'+' | b' ' | b'(' | b')' | b'-'))
        && phone.bytes().any(|b| b.is_ascii_digit());
    if ok {
        Ok(())
    } else {
        Err(DonationError::BadField("phone"))
    }
}

impl DonationMethod {
    /// Check every field rule and the per-kind rule.
    ///
    /// # Errors
    ///
    /// [`DonationError::BadField`] or [`DonationError::KindMismatch`].
    pub fn validate(&self) -> Result<(), DonationError> {
        check_id(&self.id)?;
        check_text(self.link.as_deref(), DONATION_LINK_MAX, true, "link")?;
        if self
            .link
            .as_deref()
            .is_some_and(|l| !l.starts_with("https://") || l.len() == "https://".len())
        {
            return Err(DonationError::BadField("link"));
        }
        check_text(self.coin.as_deref(), DONATION_COIN_MAX, false, "coin")?;
        check_text(self.network.as_deref(), DONATION_COIN_MAX, false, "network")?;
        check_text(self.address.as_deref(), DONATION_LINK_MAX, true, "address")?;
        let fits = if self.kind == DonationKind::Crypto {
            self.coin.is_some()
                && self.network.is_some()
                && self.address.is_some()
                && self.link.is_none()
                && self.qr_sha256.is_none()
                && !self.has_phone
        } else {
            self.icon == DonationIcon::None
                && self.coin.is_none()
                && self.network.is_none()
                && self.address.is_none()
                && (self.link.is_some() || self.qr_sha256.is_some() || self.has_phone)
        };
        if fits {
            Ok(())
        } else {
            Err(DonationError::KindMismatch)
        }
    }

    /// The bytes before the signature.
    fn body(&self) -> Result<Vec<u8>, DonationError> {
        self.validate()?;
        let mut out = Vec::with_capacity(DONATION_METHOD_MAX_LEN);
        out.push(METHOD_VERSION);
        out.extend_from_slice(&self.signed_at_unix_millis.to_be_bytes());
        put_u8_str(&mut out, &self.id)?;
        out.push(self.kind.to_byte());
        out.push(self.icon.to_byte());
        put_u16_str(&mut out, self.link.as_deref().unwrap_or(""))?;
        put_u8_str(&mut out, self.coin.as_deref().unwrap_or(""))?;
        put_u8_str(&mut out, self.network.as_deref().unwrap_or(""))?;
        put_u16_str(&mut out, self.address.as_deref().unwrap_or(""))?;
        match &self.qr_sha256 {
            Some(hash) => {
                out.push(1);
                out.extend_from_slice(hash);
            },
            None => out.push(0),
        }
        out.push(u8::from(self.has_phone));
        Ok(out)
    }

    /// Sign this method into its wire form. Used by the offline tool only.
    ///
    /// # Errors
    ///
    /// The field rules of [`DonationMethod::validate`].
    pub fn sign(&self, signing: &SigningKey) -> Result<Vec<u8>, DonationError> {
        let mut out = self.body()?;
        let sig = signing.sign(&signing_input(DOMAIN_METHOD, &out));
        out.extend_from_slice(&sig.to_bytes());
        Ok(out)
    }

    /// Parse and check the fields of a method statement WITHOUT checking its
    /// signature. For a server that has no key configured, and to read the id
    /// before deciding which key applies; a client never shows what this
    /// returns.
    ///
    /// # Errors
    ///
    /// Any layout or field error.
    pub fn decode_unverified(bytes: &[u8]) -> Result<Self, DonationError> {
        Ok(parse_method(bytes)?.0)
    }

    /// Parse, check every field and verify the signature.
    ///
    /// # Errors
    ///
    /// Any layout or field error, then [`DonationError::BadSignature`].
    pub fn open(bytes: &[u8], verifying: &VerifyingKey) -> Result<Self, DonationError> {
        let (method, body_len) = parse_method(bytes)?;
        verify_sig(DOMAIN_METHOD, bytes, body_len, verifying)?;
        Ok(method)
    }
}

impl DonationPhone {
    /// Check the id and the phone rule.
    ///
    /// # Errors
    ///
    /// [`DonationError::BadField`].
    pub fn validate(&self) -> Result<(), DonationError> {
        check_id(&self.id)?;
        check_phone(&self.phone)
    }

    /// Sign into the wire form. Used by the offline tool only.
    ///
    /// # Errors
    ///
    /// The field rules of [`DonationPhone::validate`].
    pub fn sign(&self, signing: &SigningKey) -> Result<Vec<u8>, DonationError> {
        self.validate()?;
        let mut out = Vec::with_capacity(DONATION_PHONE_MAX_LEN);
        out.push(PHONE_VERSION);
        put_u8_str(&mut out, &self.id)?;
        put_u8_str(&mut out, &self.phone)?;
        let sig = signing.sign(&signing_input(DOMAIN_PHONE, &out));
        out.extend_from_slice(&sig.to_bytes());
        Ok(out)
    }

    /// Parse and check the fields WITHOUT checking the signature (see
    /// [`DonationMethod::decode_unverified`]).
    ///
    /// # Errors
    ///
    /// Any layout or field error.
    pub fn decode_unverified(bytes: &[u8]) -> Result<Self, DonationError> {
        Ok(parse_phone(bytes)?.0)
    }

    /// Parse, check and verify the signature.
    ///
    /// # Errors
    ///
    /// Any layout or field error, then [`DonationError::BadSignature`].
    pub fn open(bytes: &[u8], verifying: &VerifyingKey) -> Result<Self, DonationError> {
        let (phone, body_len) = parse_phone(bytes)?;
        verify_sig(DOMAIN_PHONE, bytes, body_len, verifying)?;
        Ok(phone)
    }
}

/// Sign a method statement (the offline tool).
///
/// # Errors
///
/// The field rules of [`DonationMethod::validate`].
pub fn sign_donation_method(
    method: &DonationMethod,
    signing: &SigningKey,
) -> Result<Vec<u8>, DonationError> {
    method.sign(signing)
}

/// Sign a phone statement for method `id` (the offline tool).
///
/// # Errors
///
/// The field rules of [`DonationPhone::validate`].
pub fn sign_donation_phone(
    id: &str,
    phone: &str,
    signing: &SigningKey,
) -> Result<Vec<u8>, DonationError> {
    DonationPhone {
        id: id.to_string(),
        phone: phone.to_string(),
    }
    .sign(signing)
}

/// Verify a method statement against the pinned donations key.
///
/// # Errors
///
/// See [`DonationMethod::open`].
pub fn verify_donation_method(
    bytes: &[u8],
    verifying: &VerifyingKey,
) -> Result<DonationMethod, DonationError> {
    DonationMethod::open(bytes, verifying)
}

/// Verify a phone statement against the pinned donations key and the id of
/// the method it was asked for; returns the number.
///
/// # Errors
///
/// See [`DonationPhone::open`], then [`DonationError::IdMismatch`] when the
/// statement names another method.
pub fn verify_donation_phone(
    bytes: &[u8],
    verifying: &VerifyingKey,
    expected_id: &str,
) -> Result<String, DonationError> {
    let phone = DonationPhone::open(bytes, verifying)?;
    if phone.id != expected_id {
        return Err(DonationError::IdMismatch);
    }
    Ok(phone.phone)
}

fn signing_input(domain: &[u8], body: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(domain.len() + body.len());
    buf.extend_from_slice(domain);
    buf.extend_from_slice(body);
    buf
}

fn verify_sig(
    domain: &[u8],
    bytes: &[u8],
    body_len: usize,
    verifying: &VerifyingKey,
) -> Result<(), DonationError> {
    let sig_bytes = bytes.get(body_len..).ok_or(DonationError::BadLength)?;
    let sig = Signature::from_bytes(sig_bytes).map_err(|_| DonationError::BadSignature)?;
    verifying
        .verify(&signing_input(domain, &bytes[..body_len]), &sig)
        .map_err(|_| DonationError::BadSignature)
}

fn put_u8_str(out: &mut Vec<u8>, s: &str) -> Result<(), DonationError> {
    let len = u8::try_from(s.len()).map_err(|_| DonationError::BadLength)?;
    out.push(len);
    out.extend_from_slice(s.as_bytes());
    Ok(())
}

fn put_u16_str(out: &mut Vec<u8>, s: &str) -> Result<(), DonationError> {
    let len = u16::try_from(s.len()).map_err(|_| DonationError::BadLength)?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(s.as_bytes());
    Ok(())
}

/// A bounds-checked reader over the statement bytes.
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], DonationError> {
        let end = self.pos.checked_add(n).ok_or(DonationError::BadLength)?;
        let out = self
            .bytes
            .get(self.pos..end)
            .ok_or(DonationError::BadLength)?;
        self.pos = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, DonationError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<usize, DonationError> {
        let b = self.take(2)?;
        Ok(usize::from(u16::from_be_bytes([b[0], b[1]])))
    }

    fn flag(&mut self, what: &'static str) -> Result<bool, DonationError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(DonationError::BadField(what)),
        }
    }

    /// A text field of `len` bytes: `None` when empty, bounded by `max`
    /// before any copy.
    fn text(
        &mut self,
        len: usize,
        max: usize,
        what: &'static str,
    ) -> Result<Option<String>, DonationError> {
        if len > max {
            return Err(DonationError::BadField(what));
        }
        let raw = self.take(len)?;
        if raw.is_empty() {
            return Ok(None);
        }
        std::str::from_utf8(raw)
            .map(|s| Some(s.to_string()))
            .map_err(|_| DonationError::BadField(what))
    }
}

/// Parse a method statement's layout and fields; returns the method and the
/// length of the signed body. The signature must be exactly the remainder.
fn parse_method(bytes: &[u8]) -> Result<(DonationMethod, usize), DonationError> {
    if bytes.len() > DONATION_METHOD_MAX_LEN {
        return Err(DonationError::BadLength);
    }
    let mut r = Reader::new(bytes);
    let version = r.u8()?;
    if version != METHOD_VERSION {
        return Err(DonationError::BadVersion(version));
    }
    let signed_at = r.take(8)?;
    let mut signed_at_be = [0u8; 8];
    signed_at_be.copy_from_slice(signed_at);
    let id_len = usize::from(r.u8()?);
    let id = r.text(id_len, DONATION_ID_MAX, "id")?.unwrap_or_default();
    let kind = DonationKind::from_byte(r.u8()?)?;
    let icon = DonationIcon::from_byte(r.u8()?)?;
    let link_len = r.u16()?;
    let link = r.text(link_len, DONATION_LINK_MAX, "link")?;
    let coin_len = usize::from(r.u8()?);
    let coin = r.text(coin_len, DONATION_COIN_MAX, "coin")?;
    let network_len = usize::from(r.u8()?);
    let network = r.text(network_len, DONATION_COIN_MAX, "network")?;
    let address_len = r.u16()?;
    let address = r.text(address_len, DONATION_LINK_MAX, "address")?;
    let qr_sha256 = if r.flag("qr_sha256_flag")? {
        let mut hash = [0u8; DONATION_QR_HASH_LEN];
        hash.copy_from_slice(r.take(DONATION_QR_HASH_LEN)?);
        Some(hash)
    } else {
        None
    };
    let has_phone = r.flag("has_phone")?;
    let body_len = r.pos;
    if bytes.len() != body_len + SIGNATURE_LEN {
        return Err(DonationError::BadLength);
    }
    let method = DonationMethod {
        signed_at_unix_millis: i64::from_be_bytes(signed_at_be),
        id,
        kind,
        icon,
        link,
        coin,
        network,
        address,
        qr_sha256,
        has_phone,
    };
    method.validate()?;
    Ok((method, body_len))
}

/// Parse a phone statement's layout and fields; returns it and the length of
/// the signed body.
fn parse_phone(bytes: &[u8]) -> Result<(DonationPhone, usize), DonationError> {
    if bytes.len() > DONATION_PHONE_MAX_LEN {
        return Err(DonationError::BadLength);
    }
    let mut r = Reader::new(bytes);
    let version = r.u8()?;
    if version != PHONE_VERSION {
        return Err(DonationError::BadVersion(version));
    }
    let id_len = usize::from(r.u8()?);
    let id = r.text(id_len, DONATION_ID_MAX, "id")?.unwrap_or_default();
    let phone_len = usize::from(r.u8()?);
    let phone = r
        .text(phone_len, DONATION_PHONE_MAX, "phone")?
        .unwrap_or_default();
    let body_len = r.pos;
    if bytes.len() != body_len + SIGNATURE_LEN {
        return Err(DonationError::BadLength);
    }
    let out = DonationPhone { id, phone };
    out.validate()?;
    Ok((out, body_len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AccountStatement, AccountStatementKind, ActionAttestation, BackupHead, GenesisAttestation,
        HandleCertificate, KeyResetAttestation, ReleaseManifest, UsernameCertificate,
    };
    use proptest::prelude::*;

    fn key() -> SigningKey {
        SigningKey::from_seed(&[0x11; 32])
    }

    fn sbp() -> DonationMethod {
        DonationMethod {
            signed_at_unix_millis: 1_763_000_000_000,
            id: "sbp".to_string(),
            kind: DonationKind::Sbp,
            icon: DonationIcon::None,
            link: Some("https://pay.example/sbp".to_string()),
            coin: None,
            network: None,
            address: None,
            qr_sha256: Some([0x42; 32]),
            has_phone: true,
        }
    }

    fn usdt() -> DonationMethod {
        DonationMethod {
            signed_at_unix_millis: 1_763_000_000_000,
            id: "usdt".to_string(),
            kind: DonationKind::Crypto,
            icon: DonationIcon::None,
            link: None,
            coin: Some("USDT".to_string()),
            network: Some("TRON (TRC-20)".to_string()),
            address: Some("TQn9Y2khEsLJW1ChVWFMSMeRDow5KcbLSE".to_string()),
            qr_sha256: None,
            has_phone: false,
        }
    }

    #[test]
    fn methods_round_trip() {
        let sk = key();
        let vk = sk.verifying_key();
        for m in [sbp(), usdt()] {
            let wire = sign_donation_method(&m, &sk).unwrap();
            assert_eq!(verify_donation_method(&wire, &vk).unwrap(), m);
            assert_eq!(DonationMethod::decode_unverified(&wire).unwrap(), m);
        }
    }

    #[test]
    fn phone_round_trips_and_is_bound_to_its_method() {
        let sk = key();
        let vk = sk.verifying_key();
        let wire = sign_donation_phone("sbp", "+7 900 000-00-00", &sk).unwrap();
        assert_eq!(
            verify_donation_phone(&wire, &vk, "sbp").unwrap(),
            "+7 900 000-00-00"
        );
        assert_eq!(
            verify_donation_phone(&wire, &vk, "revolut"),
            Err(DonationError::IdMismatch)
        );
    }

    #[test]
    fn the_largest_statements_fit_their_bounds() {
        let sk = key();
        let mut m = usdt();
        m.id = "a".repeat(DONATION_ID_MAX);
        m.coin = Some("c".repeat(DONATION_COIN_MAX));
        m.network = Some("n".repeat(DONATION_COIN_MAX));
        m.address = Some("x".repeat(DONATION_LINK_MAX));
        let wire = m.sign(&sk).unwrap();
        assert!(wire.len() <= DONATION_METHOD_MAX_LEN);
        let mut m = sbp();
        m.id = "a".repeat(DONATION_ID_MAX);
        m.link = Some(format!("https://{}", "l".repeat(DONATION_LINK_MAX - 8)));
        let wire = m.sign(&sk).unwrap();
        assert!(wire.len() <= DONATION_METHOD_MAX_LEN);
        assert!(m.sign(&sk).is_ok());
        let wire = sign_donation_phone(&"a".repeat(32), &"1".repeat(32), &sk).unwrap();
        assert_eq!(wire.len(), DONATION_PHONE_MAX_LEN);
    }

    #[test]
    fn per_kind_rules() {
        let sk = key();
        let mut m = sbp();
        m.address = Some("x".to_string());
        assert_eq!(m.sign(&sk), Err(DonationError::KindMismatch));
        let mut m = sbp();
        m.link = None;
        m.qr_sha256 = None;
        m.has_phone = false;
        assert_eq!(m.sign(&sk), Err(DonationError::KindMismatch));
        // A link alone, a QR alone or a phone alone is enough.
        for (link, qr, phone) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            let mut m = sbp();
            m.link = link.then(|| "https://x.example".to_string());
            m.qr_sha256 = qr.then_some([1; 32]);
            m.has_phone = phone;
            assert!(m.sign(&sk).is_ok());
        }
        // A crypto method may carry any icon; a payment method only none.
        for icon in [
            DonationIcon::None,
            DonationIcon::Btc,
            DonationIcon::Eth,
            DonationIcon::Ton,
        ] {
            let mut m = usdt();
            m.icon = icon;
            assert!(m.sign(&sk).is_ok());
            let mut m = sbp();
            m.icon = icon;
            if icon == DonationIcon::None {
                assert!(m.sign(&sk).is_ok());
            } else {
                assert_eq!(m.sign(&sk), Err(DonationError::KindMismatch));
            }
        }
        for change in 0..4 {
            let mut m = usdt();
            match change {
                0 => m.link = Some("https://x.example".to_string()),
                1 => m.qr_sha256 = Some([1; 32]),
                2 => m.has_phone = true,
                _ => m.network = None,
            }
            assert_eq!(m.sign(&sk), Err(DonationError::KindMismatch));
        }
    }

    #[test]
    fn field_rules() {
        let sk = key();
        let bad_links = [
            "http://x.example",
            "https://",
            "https://a b",
            "https://a\u{7}",
        ];
        for link in bad_links {
            let mut m = sbp();
            m.link = Some(link.to_string());
            assert_eq!(m.sign(&sk), Err(DonationError::BadField("link")), "{link}");
        }
        for id in ["", "A", "a b", "a.b", &"a".repeat(33)] {
            let mut m = sbp();
            m.id = id.to_string();
            assert_eq!(m.sign(&sk), Err(DonationError::BadField("id")), "{id}");
        }
        let mut m = usdt();
        m.address = Some("T abc".to_string());
        assert_eq!(m.sign(&sk), Err(DonationError::BadField("address")));
        let mut m = usdt();
        m.coin = Some("c".repeat(65));
        assert_eq!(m.sign(&sk), Err(DonationError::BadField("coin")));
        for phone in ["", "abc", "+7 900#", "()- +", &"1".repeat(33)] {
            assert_eq!(
                sign_donation_phone("sbp", phone, &sk),
                Err(DonationError::BadField("phone")),
                "{phone}"
            );
        }
    }

    #[test]
    fn a_flag_that_is_not_zero_or_one_is_refused() {
        let wire = sbp().sign(&key()).unwrap();
        // has_phone is the byte just before the signature.
        let mut bad = wire.clone();
        let idx = bad.len() - SIGNATURE_LEN - 1;
        bad[idx] = 2;
        assert_eq!(
            DonationMethod::decode_unverified(&bad),
            Err(DonationError::BadField("has_phone"))
        );
        let mut bad = wire;
        bad[0] = 3;
        assert_eq!(
            DonationMethod::decode_unverified(&bad),
            Err(DonationError::BadVersion(3))
        );
    }

    /// Offset of the icon byte: version, signed_at, id_len, id, kind.
    fn icon_offset(m: &DonationMethod) -> usize {
        1 + 8 + 1 + m.id.len() + 1
    }

    #[test]
    fn the_icon_byte_is_signed_and_its_list_is_closed() {
        let sk = key();
        let vk = sk.verifying_key();
        let mut m = usdt();
        m.icon = DonationIcon::Btc;
        let wire = m.sign(&sk).unwrap();
        let at = icon_offset(&m);
        assert_eq!(wire[at], 1);
        // Another known icon parses but fails the signature.
        let mut bad = wire.clone();
        bad[at] = 3;
        assert_eq!(
            DonationMethod::decode_unverified(&bad).unwrap().icon,
            DonationIcon::Ton
        );
        assert_eq!(
            DonationMethod::open(&bad, &vk),
            Err(DonationError::BadSignature)
        );
        for unknown in [4u8, 0x80, 0xFF] {
            let mut bad = wire.clone();
            bad[at] = unknown;
            assert_eq!(
                DonationMethod::decode_unverified(&bad),
                Err(DonationError::BadField("icon"))
            );
        }
        // A payment method's wire with a non-zero icon is refused before the
        // signature is looked at.
        let pay = sbp();
        let mut bad = pay.sign(&sk).unwrap();
        bad[icon_offset(&pay)] = 1;
        assert_eq!(
            DonationMethod::decode_unverified(&bad),
            Err(DonationError::KindMismatch)
        );
    }

    #[test]
    fn a_version_one_method_is_refused() {
        let sk = key();
        let m = usdt();
        let wire = m.sign(&sk).unwrap();
        // The version 1 layout: the same bytes without the icon byte.
        let at = icon_offset(&m);
        let mut v1 = wire[..at].to_vec();
        v1.extend_from_slice(&wire[at + 1..wire.len() - SIGNATURE_LEN]);
        v1[0] = 1;
        let sig = sk.sign(&signing_input(DOMAIN_METHOD, &v1)).to_bytes();
        v1.extend_from_slice(&sig);
        assert_eq!(
            DonationMethod::open(&v1, &sk.verifying_key()),
            Err(DonationError::BadVersion(1))
        );
    }

    #[test]
    fn icon_names_round_trip() {
        for icon in [
            DonationIcon::None,
            DonationIcon::Btc,
            DonationIcon::Eth,
            DonationIcon::Ton,
        ] {
            assert_eq!(DonationIcon::from_name(icon.as_str()), Some(icon));
            assert_eq!(DonationIcon::from_byte(icon.to_byte()), Ok(icon));
        }
        assert_eq!(DonationIcon::from_name("usdt"), None);
    }

    #[test]
    fn the_two_kinds_and_the_key_do_not_cross() {
        let sk = key();
        let vk = sk.verifying_key();
        let method = sbp().sign(&sk).unwrap();
        let phone = sign_donation_phone("sbp", "+7 900", &sk).unwrap();
        assert!(DonationPhone::open(&method, &vk).is_err());
        assert!(DonationMethod::open(&phone, &vk).is_err());
        let other = SigningKey::from_seed(&[0x22; 32]).verifying_key();
        assert_eq!(
            DonationMethod::open(&method, &other),
            Err(DonationError::BadSignature)
        );
        assert_eq!(
            DonationPhone::open(&phone, &other),
            Err(DonationError::BadSignature)
        );
        // A method body re-signed under the phone domain does not open.
        let body_len = method.len() - SIGNATURE_LEN;
        let mut forged = method[..body_len].to_vec();
        forged.extend_from_slice(
            &sk.sign(&signing_input(DOMAIN_PHONE, &method[..body_len]))
                .to_bytes(),
        );
        assert_eq!(
            DonationMethod::open(&forged, &vk),
            Err(DonationError::BadSignature)
        );
    }

    /// Every domain separator in this crate, written out so a new kind that
    /// reuses or prefixes one fails here.
    const ALL_DOMAINS: [&[u8]; 10] = [
        b"lakko/device-attest/v1",
        b"lakko/genesis-attest/v1",
        b"lakko/backup-head/v1",
        b"lakko/release-manifest/v1",
        b"lakko/username-cert/v1",
        b"lakko/key-reset-attest/v1",
        b"lakko/account-statement/v1",
        b"lakko/handle-cert/v1",
        DOMAIN_METHOD,
        DOMAIN_PHONE,
    ];

    #[test]
    fn every_domain_is_distinct_and_none_prefixes_another() {
        for (i, a) in ALL_DOMAINS.iter().enumerate() {
            for (j, b) in ALL_DOMAINS.iter().enumerate() {
                if i != j {
                    assert!(!b.starts_with(a), "{a:?} prefixes {b:?}");
                }
            }
        }
    }

    #[test]
    fn domains_do_not_cross_between_donations_and_every_attestation_kind() {
        // The donations key is its own key, but the separation must hold even
        // under a shared one: no foreign blob opens as a donation statement,
        // no foreign signature verifies over a donation body, and no donation
        // signature verifies inside a foreign blob.
        let sk = key();
        let vk = sk.verifying_key();
        let foreign: Vec<(&str, Vec<u8>)> = vec![
            (
                "action",
                ActionAttestation::new([0xAA; 16], [0xBB; 32], 1_000)
                    .sign(&sk)
                    .to_vec(),
            ),
            (
                "genesis",
                GenesisAttestation::new([0xAA; 16], [0xBB; 32], [0xCC; 32], 1_000)
                    .sign(&sk)
                    .to_vec(),
            ),
            (
                "backup-head",
                BackupHead::new([0xAA; 16], 7, [0xBB; 32], 1_000_000)
                    .sign(&sk)
                    .to_vec(),
            ),
            (
                "release-manifest",
                ReleaseManifest::new(10, 5, 1_000_000).sign(&sk).to_vec(),
            ),
            (
                "username-cert",
                UsernameCertificate::new([0xDD; 64], [0xAA; 16], 1_000_000)
                    .sign(&sk)
                    .to_vec(),
            ),
            (
                "key-reset",
                KeyResetAttestation::new([0xAA; 32], 2, [0xBB; 32], 1_000_000)
                    .sign(&sk)
                    .to_vec(),
            ),
            (
                "account-statement",
                AccountStatement::new(
                    AccountStatementKind::DeviceRevoke,
                    [0xAA; 16],
                    1_000_000,
                    [0x01; 16],
                    &[],
                )
                .sign(&sk)
                .to_vec(),
            ),
            (
                "handle-cert",
                HandleCertificate::new([0xDD; 64], [0xEE; 32], 1_000_000)
                    .sign(&sk)
                    .to_vec(),
            ),
        ];
        let method = sbp().sign(&sk).unwrap();
        let phone = sign_donation_phone("sbp", "+7 900", &sk).unwrap();
        for (name, wire) in &foreign {
            assert!(
                DonationMethod::open(wire, &vk).is_err(),
                "a {name} blob opened as a donation method"
            );
            assert!(
                DonationPhone::open(wire, &vk).is_err(),
                "a {name} blob opened as a donation phone"
            );
            let sig = &wire[wire.len() - SIGNATURE_LEN..];
            for own in [&method, &phone] {
                let mut spliced = own.clone();
                let len = spliced.len();
                spliced[len - SIGNATURE_LEN..].copy_from_slice(sig);
                assert!(DonationMethod::open(&spliced, &vk).is_err());
                assert!(DonationPhone::open(&spliced, &vk).is_err());
            }
        }
        let method_sig = &method[method.len() - SIGNATURE_LEN..];
        let splice = |wire: &[u8]| {
            let mut out = wire.to_vec();
            let len = out.len();
            out[len - SIGNATURE_LEN..].copy_from_slice(method_sig);
            out
        };
        assert!(ActionAttestation::open(&splice(&foreign[0].1), &vk).is_err());
        assert!(GenesisAttestation::open(&splice(&foreign[1].1), &vk).is_err());
        assert!(BackupHead::open(&splice(&foreign[2].1), &vk).is_err());
        assert!(ReleaseManifest::open(&splice(&foreign[3].1), &vk).is_err());
        assert!(UsernameCertificate::open(&splice(&foreign[4].1), &vk).is_err());
        assert!(KeyResetAttestation::open(&splice(&foreign[5].1), &vk).is_err());
        assert!(AccountStatement::open(&splice(&foreign[6].1), &vk).is_err());
        assert!(HandleCertificate::open(&splice(&foreign[7].1), &vk).is_err());
    }

    fn text_strategy(max: usize) -> impl Strategy<Value = String> {
        proptest::collection::vec(proptest::char::range('!', '~'), 1..=max)
            .prop_map(|v| v.into_iter().collect())
    }

    fn method_strategy() -> impl Strategy<Value = DonationMethod> {
        let id = "[a-z0-9_-]{1,32}";
        let crypto = (
            id,
            any::<i64>(),
            prop_oneof![
                Just(DonationIcon::None),
                Just(DonationIcon::Btc),
                Just(DonationIcon::Eth),
                Just(DonationIcon::Ton)
            ],
            text_strategy(64),
            "[A-Za-z0-9 ()-]{1,64}",
            text_strategy(512),
        )
            .prop_map(|(id, at, icon, coin, network, address)| DonationMethod {
                signed_at_unix_millis: at,
                id,
                kind: DonationKind::Crypto,
                icon,
                link: None,
                coin: Some(coin),
                network: Some(network),
                address: Some(address),
                qr_sha256: None,
                has_phone: false,
            });
        let payment = (
            id,
            any::<i64>(),
            prop_oneof![
                Just(DonationKind::Sbp),
                Just(DonationKind::Revolut),
                Just(DonationKind::Bmc)
            ],
            proptest::option::of(text_strategy(500).prop_map(|t| format!("https://{t}"))),
            proptest::option::of(any::<[u8; 32]>()),
            any::<bool>(),
        )
            .prop_filter("something to show", |(_, _, _, l, q, p)| {
                l.is_some() || q.is_some() || *p
            })
            .prop_map(|(id, at, kind, link, qr, has_phone)| DonationMethod {
                signed_at_unix_millis: at,
                id,
                kind,
                icon: DonationIcon::None,
                link,
                coin: None,
                network: None,
                address: None,
                qr_sha256: qr,
                has_phone,
            });
        prop_oneof![crypto, payment]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn any_valid_method_round_trips_and_resists_damage(m in method_strategy()) {
            let sk = key();
            let vk = sk.verifying_key();
            let wire = m.sign(&sk).unwrap();
            prop_assert!(wire.len() <= DONATION_METHOD_MAX_LEN);
            prop_assert_eq!(&verify_donation_method(&wire, &vk).unwrap(), &m);
            // The fuzz harness's round-trip assertion holds on a valid input.
            prop_assert!(crate::fuzz_api::open_donation(&wire));
            for cut in 0..wire.len() {
                prop_assert!(DonationMethod::open(&wire[..cut], &vk).is_err());
            }
            let mut longer = wire.clone();
            longer.push(0);
            prop_assert!(DonationMethod::open(&longer, &vk).is_err());
            for idx in 0..wire.len() {
                let mut bad = wire.clone();
                bad[idx] ^= 0x01;
                prop_assert!(DonationMethod::open(&bad, &vk).is_err(), "flip at {}", idx);
            }
            let other = SigningKey::from_seed(&[0x33; 32]).verifying_key();
            prop_assert_eq!(DonationMethod::open(&wire, &other), Err(DonationError::BadSignature));
        }

        #[test]
        fn any_valid_phone_round_trips_and_resists_damage(
            id in "[a-z0-9_-]{1,32}",
            phone in "[0-9][0-9 ()+-]{0,31}",
        ) {
            let sk = key();
            let vk = sk.verifying_key();
            let wire = sign_donation_phone(&id, &phone, &sk).unwrap();
            prop_assert_eq!(verify_donation_phone(&wire, &vk, &id).unwrap(), phone);
            prop_assert!(crate::fuzz_api::open_donation(&wire));
            for cut in 0..wire.len() {
                prop_assert!(DonationPhone::open(&wire[..cut], &vk).is_err());
            }
            for idx in 0..wire.len() {
                let mut bad = wire.clone();
                bad[idx] ^= 0x01;
                prop_assert!(verify_donation_phone(&bad, &vk, &id).is_err(), "flip at {}", idx);
            }
        }

        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..1400)) {
            let vk = key().verifying_key();
            let _ = DonationMethod::open(&bytes, &vk);
            let _ = DonationPhone::open(&bytes, &vk);
            let _ = crate::fuzz_api::open_donation(&bytes);
            if let Ok(m) = DonationMethod::decode_unverified(&bytes) {
                prop_assert!(m.validate().is_ok());
            }
        }
    }
}
