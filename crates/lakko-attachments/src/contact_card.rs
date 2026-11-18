// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A contact card (ADR-0090 section 7): a display name, a canonical username
//! and an account key. Nothing in it lets the receiver write to the person
//! described: no reach grant, no mailbox binding. The receiver adds the
//! person through the ordinary username lookup and accepts the result only
//! when the discovered account key equals [`ContactCard::account_key`].
//!
//! ```text
//! contact_card = { "name": display name (lakko_contacts::check_display_name),
//!                  "username": canonical username,
//!                  "account_key": standard base64 of 32 bytes }
//! ```

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_contacts::username::check_canonical_username;
use serde::{Deserialize, Serialize};

use crate::content::{check_display_name_field, ContentError, TextError, ACCOUNT_KEY_LEN};

/// A contact card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContactCard {
    /// The contact's display name as the sender knows it.
    pub name: String,
    /// The contact's canonical username.
    pub username: String,
    /// The contact's account signature key.
    pub account_key: [u8; ACCOUNT_KEY_LEN],
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContactCardWire {
    name: String,
    username: String,
    account_key: String,
}

impl ContactCard {
    /// Check every rule.
    ///
    /// # Errors
    ///
    /// The first [`ContentError`] that applies.
    pub fn validate(&self) -> Result<(), ContentError> {
        check_display_name_field(&self.name, "name")?;
        check_canonical_username(&self.username).map_err(|_| ContentError::Text {
            field: "username",
            reason: TextError::NotAUsername,
        })?;
        Ok(())
    }

    /// The envelope's JSON value.
    ///
    /// # Errors
    ///
    /// What [`Self::validate`] refuses.
    pub fn to_json(&self) -> Result<serde_json::Value, ContentError> {
        self.validate()?;
        serde_json::to_value(ContactCardWire {
            name: self.name.clone(),
            username: self.username.clone(),
            account_key: B64.encode(self.account_key),
        })
        .map_err(|_| ContentError::Malformed {
            what: "contactCard",
        })
    }

    /// Decode from the envelope's JSON value.
    ///
    /// # Errors
    ///
    /// [`ContentError::Malformed`] for a wrong shape or a key that is not
    /// standard base64 of 32 bytes, or what [`Self::validate`] refuses.
    pub fn from_json(value: serde_json::Value) -> Result<Self, ContentError> {
        let malformed = ContentError::Malformed {
            what: "contactCard",
        };
        let wire: ContactCardWire = serde_json::from_value(value).map_err(|_| malformed)?;
        if wire.account_key.len() != ACCOUNT_KEY_LEN.div_ceil(3) * 4 {
            return Err(malformed);
        }
        let account_key: [u8; ACCOUNT_KEY_LEN] = B64
            .decode(&wire.account_key)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or(malformed)?;
        let card = Self {
            name: wire.name,
            username: wire.username,
            account_key,
        };
        card.validate()?;
        Ok(card)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn round_trip(name in "[A-Za-z][A-Za-z é]{0,30}[a-z]", username in "[a-z0-9_]{3,32}", key in any::<[u8; 32]>()) {
            let c = ContactCard { name, username, account_key: key };
            let v = c.to_json().unwrap();
            prop_assert_eq!(ContactCard::from_json(v).unwrap(), c);
        }

        #[test]
        fn arbitrary_json_strings_never_panic(name in ".*", username in ".*", key in ".*") {
            let v = serde_json::json!({"name": name, "username": username, "account_key": key});
            if let Ok(c) = ContactCard::from_json(v) {
                prop_assert!(c.validate().is_ok());
            }
        }
    }

    #[test]
    fn rules() {
        let key = B64.encode([1u8; 32]);
        let ok = serde_json::json!({"name": "Ann", "username": "ann_1", "account_key": key});
        assert!(ContactCard::from_json(ok).is_ok());
        let bad_user = serde_json::json!({"name": "Ann", "username": "Ann", "account_key": key});
        assert!(ContactCard::from_json(bad_user).is_err());
        let bad_name = serde_json::json!({"name": " Ann", "username": "ann", "account_key": key});
        assert!(ContactCard::from_json(bad_name).is_err());
        let short_key = serde_json::json!({"name": "Ann", "username": "ann", "account_key": B64.encode([1u8; 31])});
        assert!(ContactCard::from_json(short_key).is_err());
        let extra =
            serde_json::json!({"name": "Ann", "username": "ann", "account_key": key, "reach": 1});
        assert!(ContactCard::from_json(extra).is_err());
    }
}
