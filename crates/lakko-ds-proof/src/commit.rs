// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Register an epoch and append its commit row in one step ([ADR-0081]).
//!
//! A commit that moves a journal to epoch `N + 1` reaches the Delivery
//! Service as ONE request, `POST /ds/group/{hex}/commit`:
//!
//! - the body carries an append-mode register-epoch statement for `N + 1`
//!   ([`crate::statement`], signed by the journal's root or a registrar, as
//!   for `PUT .../access`) and the journal row (the commit, in whatever
//!   envelope the group's journal uses);
//! - the request proof ([`crate::DsProofSigner`]) is signed under the key of
//!   epoch `N`, over this exact body.
//!
//! The DS registers the key and appends the row in one transaction and keeps
//! [`commit_row_hash`] of the row with the registration, so a key registered
//! without its commit row cannot exist. Re-sending the same row under the
//! same key appends nothing and answers the original message id; any other
//! append at an existing epoch is `409`.
//!
//! # Body
//!
//! ```text
//! {"statement_b64":"<standard base64>","row_b64":"<standard base64>"}
//! ```
//!
//! Built here ([`commit_body_json`]) so no client spells a field name. The
//! statement is re-signed on every attempt (its nonce is spent, its time
//! window is two minutes); the row is the same bytes every time.
//!
//! [ADR-0081]: ../../../docs/adr/0081-devicegroup-commits-cannot-strand.md

use base64::engine::general_purpose::STANDARD as B64_STD;
use base64::Engine as _;
use lakko_crypto::hash;

use crate::statement::{
    RegisterMode, SignedStatement, StatementBody, StatementError, StatementSigner, JOURNAL_ID_LEN,
    KEY_LEN,
};
use crate::NONCE_LEN;

/// The last path segment of the route: `POST /ds/group/{hex}/commit`.
pub const COMMIT_PATH_SEGMENT: &str = "commit";

/// The JSON field carrying the statement.
pub const COMMIT_STATEMENT_FIELD: &str = "statement_b64";

/// The JSON field carrying the row.
pub const COMMIT_ROW_FIELD: &str = "row_b64";

/// The digest the DS keeps with a registration made through the commit
/// route: `BLAKE3(row)`. An identical re-send is recognised by it.
#[must_use]
pub fn commit_row_hash(row: &[u8]) -> [u8; 32] {
    hash::blake3(row)
}

/// Why a commit body was not built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CommitBodyError {
    /// The statement is not an append-mode register-epoch statement.
    #[error("a commit carries an append-mode register-epoch statement")]
    NotAnAppend,
    /// The row is empty.
    #[error("a commit row must be non-empty")]
    EmptyRow,
}

/// Whether `statement` is one the commit route takes: a register-epoch
/// statement in append mode.
#[must_use]
pub const fn is_commit_statement(statement: &SignedStatement) -> bool {
    matches!(
        statement.body,
        StatementBody::RegisterEpoch {
            mode: RegisterMode::Append,
            ..
        }
    )
}

/// The exact JSON body of `POST /ds/group/{hex}/commit` for `statement` and
/// `row`. The request proof is signed over these bytes.
///
/// # Errors
///
/// [`CommitBodyError::NotAnAppend`] when the statement is not an append-mode
/// register-epoch statement, [`CommitBodyError::EmptyRow`] for an empty row.
pub fn commit_body_json(
    statement: &SignedStatement,
    row: &[u8],
) -> Result<String, CommitBodyError> {
    if !is_commit_statement(statement) {
        return Err(CommitBodyError::NotAnAppend);
    }
    if row.is_empty() {
        return Err(CommitBodyError::EmptyRow);
    }
    Ok(format!(
        "{{\"{COMMIT_STATEMENT_FIELD}\":\"{}\",\"{COMMIT_ROW_FIELD}\":\"{}\"}}",
        B64_STD.encode(statement.encode()),
        B64_STD.encode(row)
    ))
}

impl StatementSigner {
    /// Sign the append-mode registration of `epoch_key` at `epoch` that a
    /// commit carries, revoking `revoke` in the same step (the rules are
    /// those of `PUT .../access` in append mode).
    ///
    /// # Errors
    ///
    /// The list rules of a register-epoch statement (see
    /// [`crate::sign_statement`]).
    pub fn sign_commit_at(
        &self,
        journal_id: &[u8; JOURNAL_ID_LEN],
        epoch: u64,
        epoch_key: [u8; KEY_LEN],
        revoke: Vec<[u8; KEY_LEN]>,
        now_ms: u64,
        nonce: [u8; NONCE_LEN],
    ) -> Result<SignedStatement, StatementError> {
        self.sign_at(
            journal_id,
            StatementBody::RegisterEpoch {
                epoch,
                epoch_key,
                mode: RegisterMode::Append,
                revoke,
            },
            now_ms,
            nonce,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SigningKey;

    const JOURNAL: [u8; 32] = [0x4B; 32];

    fn pk(seed: u8) -> [u8; 32] {
        SigningKey::from_seed(&[seed; 32])
            .verifying_key()
            .to_bytes()
    }

    fn decode_field<'a>(json: &'a str, field: &str) -> &'a str {
        let marker = format!("\"{field}\":\"");
        let start = json.find(&marker).unwrap() + marker.len();
        let end = start + json[start..].find('"').unwrap();
        &json[start..end]
    }

    #[test]
    fn the_body_carries_the_statement_and_the_row() {
        let signer = StatementSigner::from_seed(&[0x31; 32]);
        let statement = signer
            .sign_commit_at(&JOURNAL, 5, pk(2), vec![pk(3)], 1_700_000_000_000, [7; 16])
            .unwrap();
        assert!(is_commit_statement(&statement));
        let body = commit_body_json(&statement, b"row bytes").unwrap();
        let blob = B64_STD
            .decode(decode_field(&body, COMMIT_STATEMENT_FIELD))
            .unwrap();
        assert_eq!(SignedStatement::decode(&blob).unwrap(), statement);
        assert_eq!(
            B64_STD
                .decode(decode_field(&body, COMMIT_ROW_FIELD))
                .unwrap(),
            b"row bytes"
        );
        statement.verify(1_700_000_000_000).unwrap();
    }

    #[test]
    fn only_an_append_registration_and_a_row_make_a_body() {
        let signer = StatementSigner::from_seed(&[0x32; 32]);
        let replace = signer
            .sign_at(
                &JOURNAL,
                StatementBody::RegisterEpoch {
                    epoch: 5,
                    epoch_key: pk(2),
                    mode: RegisterMode::Replace,
                    revoke: vec![],
                },
                1,
                [1; 16],
            )
            .unwrap();
        assert_eq!(
            commit_body_json(&replace, b"row"),
            Err(CommitBodyError::NotAnAppend)
        );
        let claim = signer
            .sign_at(
                &JOURNAL,
                StatementBody::Claim {
                    epoch: 1,
                    epoch_key: pk(2),
                },
                1,
                [2; 16],
            )
            .unwrap();
        assert_eq!(
            commit_body_json(&claim, b"row"),
            Err(CommitBodyError::NotAnAppend)
        );
        let append = signer
            .sign_commit_at(&JOURNAL, 5, pk(2), vec![], 1, [3; 16])
            .unwrap();
        assert_eq!(
            commit_body_json(&append, b""),
            Err(CommitBodyError::EmptyRow)
        );
    }

    #[test]
    fn the_row_hash_is_blake3_of_the_row() {
        assert_eq!(commit_row_hash(b"abc"), hash::blake3(b"abc"));
        assert_ne!(commit_row_hash(b"abc"), commit_row_hash(b"abd"));
    }
}
