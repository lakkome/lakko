// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A public channel's ban list ([ADR-0087] section 13a, "Banning").
//!
//! The channel service holds no key to refuse a comment with, by design: a
//! comment names only a post key the service cannot link to anyone. In a
//! private channel a ban is the existing removal (no seed from the next
//! epoch). In a public channel reading cannot be refused, so a ban is a list
//! of comment author keys ([`crate::comment`]) every reader hides the
//! comments of. Reactions cannot be banned: nobody knows whose they are.
//!
//! ## How it travels
//!
//! Like the metadata record ([`crate::meta`]): the list is the **plaintext of
//! an ordinary post**, sealed under the epoch seed and signed by a publisher
//! key through [`crate::post::compose_post`], so it is "sealed and
//! publisher-signed" with no cryptography of its own here. A reader runs
//! [`crate::classify_post_plaintext`] on every post; a ban-list domain match
//! is [`crate::PostContent::BanList`] (or
//! [`crate::PostContent::MalformedBanList`], dropped), never shown as a post.
//! A reader keeps the newest list per channel by `updated_at_ms`
//! ([`ban_list_is_newer`], strictly greater), and a publisher re-issues the
//! list after every rotation with its **stored** stamp, as the metadata
//! record is re-issued, so a late subscriber always holds the seed of an
//! epoch that carries one.
//!
//! ## Wire
//!
//! ```text
//! "lakko/channel-ban-list/v1\0" ‖ version(1)=0x01 ‖ updated_at_ms(8, BE) ‖ count(2, BE) ‖ count × author_key(32)
//! ```
//!
//! The keys are in the order they were banned, **oldest first**, with no
//! repeat; `count` is at most [`BAN_LIST_MAX_KEYS`] (1,024, chosen). When a
//! ban would pass the cap the oldest key goes ([`BanList::with_banned`]). An
//! empty list is valid (every ban lifted). Any violation refuses the whole
//! record; [`compose_ban_list`] refuses exactly what [`decode_ban_list`]
//! refuses.
//!
//! The domain ends in NUL, so it can never be the start of a chat envelope
//! (JSON, which opens with `{`), and it differs from the metadata domain in
//! its 15th byte.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use thiserror::Error;

use crate::comment::COMMENT_AUTHOR_KEY_LEN;

/// Domain separator the ban list starts with. Frozen.
pub const BAN_LIST_DOMAIN_V1: &[u8] = b"lakko/channel-ban-list/v1\x00";
/// The schema version this build writes and accepts.
pub const BAN_LIST_VERSION_V1: u8 = 0x01;
/// The most author keys a list holds (ADR-0087 section 13a, chosen).
pub const BAN_LIST_MAX_KEYS: usize = 1024;

const FIXED_PAYLOAD_LEN: usize = 1 + 8 + 2;

/// The shortest valid list: no key.
pub const BAN_LIST_MIN_LEN: usize = BAN_LIST_DOMAIN_V1.len() + FIXED_PAYLOAD_LEN;
/// The longest valid list.
pub const BAN_LIST_MAX_LEN: usize = BAN_LIST_MIN_LEN + BAN_LIST_MAX_KEYS * COMMENT_AUTHOR_KEY_LEN;

/// A comment author key.
pub type AuthorKey = [u8; COMMENT_AUTHOR_KEY_LEN];

/// A channel's ban list, as a publisher stated it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BanList {
    /// Publisher-clock UNIX milliseconds at which the list was set. Orders
    /// lists of one channel ([`ban_list_is_newer`]).
    pub updated_at_ms: u64,
    /// The banned author keys, oldest first, no repeat.
    pub authors: Vec<AuthorKey>,
}

/// Why a ban list was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BanListError {
    /// The bytes do not start with [`BAN_LIST_DOMAIN_V1`]. Only from
    /// [`decode_ban_list`].
    #[error("plaintext does not carry the ban-list domain separator")]
    WrongDomain,
    /// The version byte is not [`BAN_LIST_VERSION_V1`].
    #[error("ban list carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The record ends before a declared field.
    #[error("ban list is truncated")]
    Truncated,
    /// Bytes remain after the last key.
    #[error("ban list has trailing bytes")]
    TrailingBytes,
    /// More than [`BAN_LIST_MAX_KEYS`] keys.
    #[error("ban list holds more than {max} keys")]
    TooManyKeys {
        /// The cap.
        max: usize,
    },
    /// A key appears twice.
    #[error("ban list names a key twice")]
    DuplicateKey,
}

impl BanListError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::Truncated => "truncated",
            Self::TrailingBytes => "trailingBytes",
            Self::TooManyKeys { .. } => "tooManyKeys",
            Self::DuplicateKey => "duplicateKey",
        }
    }
}

impl BanList {
    /// An empty list stamped `updated_at_ms`.
    #[must_use]
    pub const fn empty(updated_at_ms: u64) -> Self {
        Self {
            updated_at_ms,
            authors: Vec::new(),
        }
    }

    /// Check every bound: the one rule compose and decode both run.
    ///
    /// # Errors
    ///
    /// [`BanListError::TooManyKeys`] or [`BanListError::DuplicateKey`].
    pub fn validate(&self) -> Result<(), BanListError> {
        if self.authors.len() > BAN_LIST_MAX_KEYS {
            return Err(BanListError::TooManyKeys {
                max: BAN_LIST_MAX_KEYS,
            });
        }
        let mut sorted = self.authors.clone();
        sorted.sort_unstable();
        if sorted.windows(2).any(|w| w[0] == w[1]) {
            return Err(BanListError::DuplicateKey);
        }
        Ok(())
    }

    /// Whether comments by `author` are hidden.
    #[must_use]
    pub fn is_banned(&self, author: &AuthorKey) -> bool {
        self.authors.contains(author)
    }

    /// The list with `author` banned, stamped above this one (at `now_ms`, or
    /// one millisecond past the current stamp if the clock is behind it). A
    /// key already banned keeps its place; a new one goes last, and the
    /// oldest key leaves when the list would pass [`BAN_LIST_MAX_KEYS`].
    #[must_use]
    pub fn with_banned(&self, author: AuthorKey, now_ms: u64) -> Self {
        let mut authors = self.authors.clone();
        if !authors.contains(&author) {
            authors.push(author);
        }
        if authors.len() > BAN_LIST_MAX_KEYS {
            let excess = authors.len() - BAN_LIST_MAX_KEYS;
            authors.drain(..excess);
        }
        Self {
            updated_at_ms: self.next_stamp(now_ms),
            authors,
        }
    }

    /// The list with `author` no longer banned, stamped as
    /// [`Self::with_banned`] stamps.
    #[must_use]
    pub fn without(&self, author: &AuthorKey, now_ms: u64) -> Self {
        Self {
            updated_at_ms: self.next_stamp(now_ms),
            authors: self
                .authors
                .iter()
                .filter(|a| *a != author)
                .copied()
                .collect(),
        }
    }

    const fn next_stamp(&self, now_ms: u64) -> u64 {
        let floor = self.updated_at_ms.saturating_add(1);
        if now_ms > floor {
            now_ms
        } else {
            floor
        }
    }
}

/// `true` when a list stamped `incoming_updated_at_ms` must replace the one
/// stamped `stored_updated_at_ms` for the same channel. Strictly greater;
/// equal is not newer, so a re-issue is not an update.
#[must_use]
pub const fn ban_list_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    incoming_updated_at_ms > stored_updated_at_ms
}

/// Compose a ban list: the PLAINTEXT handed to [`crate::post::compose_post`].
///
/// # Errors
///
/// Exactly [`BanList::validate`]'s errors.
pub fn compose_ban_list(list: &BanList) -> Result<Vec<u8>, BanListError> {
    list.validate()?;
    let count = u16::try_from(list.authors.len()).map_err(|_| BanListError::TooManyKeys {
        max: BAN_LIST_MAX_KEYS,
    })?;
    let mut out =
        Vec::with_capacity(BAN_LIST_MIN_LEN + list.authors.len() * COMMENT_AUTHOR_KEY_LEN);
    out.extend_from_slice(BAN_LIST_DOMAIN_V1);
    out.push(BAN_LIST_VERSION_V1);
    out.extend_from_slice(&list.updated_at_ms.to_be_bytes());
    out.extend_from_slice(&count.to_be_bytes());
    for a in &list.authors {
        out.extend_from_slice(a);
    }
    Ok(out)
}

/// Strict decoder: a plaintext that is not a ban list is a hard error.
/// Readers normally call [`crate::classify_post_plaintext`].
///
/// # Errors
///
/// Every [`BanListError`]: the domain first, then the version, then the
/// count against its cap (before anything is copied), then the length, then
/// repeats.
pub fn decode_ban_list(bytes: &[u8]) -> Result<BanList, BanListError> {
    let payload = bytes
        .strip_prefix(BAN_LIST_DOMAIN_V1)
        .ok_or(BanListError::WrongDomain)?;
    decode_payload(payload)
}

pub(crate) fn decode_payload(payload: &[u8]) -> Result<BanList, BanListError> {
    let (&version, rest) = payload.split_first().ok_or(BanListError::Truncated)?;
    if version != BAN_LIST_VERSION_V1 {
        return Err(BanListError::UnknownVersion(version));
    }
    if rest.len() < 10 {
        return Err(BanListError::Truncated);
    }
    let (stamp, rest) = rest.split_at(8);
    let (count, keys) = rest.split_at(2);
    let updated_at_ms = u64::from_be_bytes(stamp.try_into().map_err(|_| BanListError::Truncated)?);
    let count = usize::from(u16::from_be_bytes(
        count.try_into().map_err(|_| BanListError::Truncated)?,
    ));
    if count > BAN_LIST_MAX_KEYS {
        return Err(BanListError::TooManyKeys {
            max: BAN_LIST_MAX_KEYS,
        });
    }
    let need = count * COMMENT_AUTHOR_KEY_LEN;
    if keys.len() < need {
        return Err(BanListError::Truncated);
    }
    if keys.len() > need {
        return Err(BanListError::TrailingBytes);
    }
    let authors = keys
        .chunks_exact(COMMENT_AUTHOR_KEY_LEN)
        .map(|c| {
            let mut k = [0u8; COMMENT_AUTHOR_KEY_LEN];
            k.copy_from_slice(c);
            k
        })
        .collect();
    let list = BanList {
        updated_at_ms,
        authors,
    };
    list.validate()?;
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u16) -> AuthorKey {
        let mut k = [0u8; 32];
        k[..2].copy_from_slice(&n.to_be_bytes());
        k[31] = 0xA5;
        k
    }

    #[test]
    fn the_layout_is_pinned_and_round_trips() {
        let list = BanList {
            updated_at_ms: 5,
            authors: vec![key(2), key(1)],
        };
        let wire = compose_ban_list(&list).unwrap();
        assert_eq!(BAN_LIST_DOMAIN_V1.len(), 26);
        assert_eq!(wire.len(), BAN_LIST_MIN_LEN + 64);
        assert_eq!(wire[26], 0x01);
        assert_eq!(&wire[27..35], &5u64.to_be_bytes());
        assert_eq!(&wire[35..37], &2u16.to_be_bytes());
        // Order is kept: oldest first, not sorted.
        assert_eq!(&wire[37..69], &key(2));
        assert_eq!(decode_ban_list(&wire).unwrap(), list);
        let empty = compose_ban_list(&BanList::empty(9)).unwrap();
        assert_eq!(empty.len(), BAN_LIST_MIN_LEN);
        assert_eq!(decode_ban_list(&empty).unwrap(), BanList::empty(9));
        assert_eq!(BAN_LIST_MAX_LEN, 37 + 1024 * 32);
    }

    #[test]
    fn decode_refuses_every_malformed_shape_and_compose_the_same() {
        let wire = compose_ban_list(&BanList {
            updated_at_ms: 5,
            authors: vec![key(1), key(2)],
        })
        .unwrap();
        for cut in 0..wire.len() {
            assert!(decode_ban_list(&wire[..cut]).is_err(), "{cut}");
        }
        let mut t = wire.clone();
        t.push(0);
        assert_eq!(decode_ban_list(&t), Err(BanListError::TrailingBytes));
        let mut v = wire.clone();
        v[26] = 2;
        assert_eq!(decode_ban_list(&v), Err(BanListError::UnknownVersion(2)));
        let mut c = wire.clone();
        c[35..37].copy_from_slice(&1025u16.to_be_bytes());
        assert_eq!(
            decode_ban_list(&c),
            Err(BanListError::TooManyKeys { max: 1024 })
        );
        let dup = BanList {
            updated_at_ms: 5,
            authors: vec![key(1), key(1)],
        };
        assert_eq!(compose_ban_list(&dup), Err(BanListError::DuplicateKey));
        let mut d = wire[..37].to_vec();
        d.extend_from_slice(&key(1));
        d.extend_from_slice(&key(1));
        assert_eq!(decode_ban_list(&d), Err(BanListError::DuplicateKey));
        assert_eq!(
            decode_ban_list(b"lakko/channel-meta/v1\x00\x03"),
            Err(BanListError::WrongDomain)
        );
        let many = BanList {
            updated_at_ms: 0,
            authors: (0..1025).map(key).collect(),
        };
        assert_eq!(
            compose_ban_list(&many),
            Err(BanListError::TooManyKeys { max: 1024 })
        );
    }

    #[test]
    fn banning_appends_lifts_and_drops_the_oldest_at_the_cap() {
        let list = BanList::empty(100);
        let one = list.with_banned(key(1), 50);
        assert_eq!(one.authors, vec![key(1)]);
        // A clock behind the stamp still moves it forward.
        assert_eq!(one.updated_at_ms, 101);
        let again = one.with_banned(key(1), 500);
        assert_eq!(again.authors, vec![key(1)]);
        assert_eq!(again.updated_at_ms, 500);
        assert!(again.is_banned(&key(1)));
        let lifted = again.without(&key(1), 400);
        assert!(lifted.authors.is_empty());
        assert_eq!(lifted.updated_at_ms, 501);
        assert!(ban_list_is_newer(lifted.updated_at_ms, again.updated_at_ms));
        assert!(!ban_list_is_newer(5, 5));

        let mut full = BanList::empty(0);
        for n in 0..1024 {
            full = full.with_banned(key(n), u64::from(n) + 1);
        }
        assert_eq!(full.authors.len(), 1024);
        let next = full.with_banned(key(5000), 9999);
        assert_eq!(next.authors.len(), 1024);
        assert!(!next.is_banned(&key(0)));
        assert_eq!(next.authors[0], key(1));
        assert_eq!(next.authors[1023], key(5000));
        assert!(compose_ban_list(&next).is_ok());
    }
}
