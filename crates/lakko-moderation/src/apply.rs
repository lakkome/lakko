// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The one check a service runs before it applies a moderator's statement
//! ([ADR-0098] sections 4 and 5, step 5).
//!
//! Every applying service (auth, discovery, channels, storage) and the
//! moderation service itself chain the same primitives: the moderator list
//! verifies under the pinned root, the signed statement decodes and verifies
//! under the list, its action is one the service applies, and the reason and
//! salt handed beside it open its commitment. Written once here, in that
//! order, with one refusal code per step.
//!
//! The internal statement body is the same for every applying service
//! ([`StatementBody`]): `{"signed_statement_b64", "moderator_list_b64",
//! "log_index", "reason", "reason_salt_b64"}`, base64 standard (any of the
//! four common spellings is read). It names no account key: an account is
//! found by its target.
//!
//! Freshness: a statement is accepted only within [`MAX_STATEMENT_AGE_MS`]
//! after it was signed and no more than [`MAX_STATEMENT_FUTURE_MS`] before
//! (by the applying service's clock), checked after the signature. A
//! statement the moderation service failed to forward must therefore be
//! resent within a day of signing; after that the moderator signs a new one.
//!
//! The list floor: every applying service keeps the newest moderator-list
//! version it has seen, raised by any newer list that verifies under the
//! pinned root (a statement body's list, or `POST
//! /internal/moderation/list` with a [`ListBody`]) even when the statement
//! beside it is then refused; a list older than the floor is
//! [`ApplyError::StaleList`]. [`list_floor`] is the comparison, written once.
//! The service raises the floor before it checks the statement, so a revoked
//! moderator's statement under an old list is refused once any newer list
//! reached it.
//!
//! What stays with each service: its replay table (a statement id applied
//! once), the stored list floor, and the per-target order (a statement older
//! by `issued_at_ms` than the newest one applied to the same target changes
//! nothing).
//!
//! [ADR-0098]: ../../../docs/adr/0098-reporting-and-moderation.md

use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::moderators::ModeratorList;
use crate::reason::Reason;
use crate::statement::{Action, SignedStatement, Statement};
use crate::ModerationError;

/// Every action, in wire order.
pub const ALL_ACTIONS: &[Action] = &[
    Action::Suspend,
    Action::Lift,
    Action::TakeBackName,
    Action::ReleaseName,
    Action::HideFromSearch,
    Action::ShowInSearch,
    Action::ChannelDelist,
    Action::ChannelBlockHandle,
    Action::ChannelRemove,
    Action::ChannelRestore,
    Action::GroupDelist,
    Action::GroupBlockHandle,
    Action::GroupRestore,
    Action::BlobDelete,
    Action::KeepSuspension,
];

/// How far ahead of the applying service's clock a statement's
/// `issued_at_ms` may be (five minutes). Chosen, not measured.
pub const MAX_STATEMENT_FUTURE_MS: u64 = 5 * 60 * 1000;
/// How long after its `issued_at_ms` a statement is still applied (a day).
/// Chosen, not measured.
pub const MAX_STATEMENT_AGE_MS: u64 = 24 * 60 * 60 * 1000;

/// The service that applies an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApplyingService {
    /// Suspend, Lift, KeepSuspension.
    Auth,
    /// Names, search, group handles.
    Discovery,
    /// Channels.
    Channels,
    /// Stored blobs.
    Storage,
}

impl ApplyingService {
    /// The actions this service applies.
    #[must_use]
    pub const fn actions(self) -> &'static [Action] {
        match self {
            Self::Auth => &[Action::Suspend, Action::Lift, Action::KeepSuspension],
            Self::Discovery => &[
                Action::TakeBackName,
                Action::ReleaseName,
                Action::HideFromSearch,
                Action::ShowInSearch,
                Action::GroupDelist,
                Action::GroupBlockHandle,
                Action::GroupRestore,
            ],
            Self::Channels => &[
                Action::ChannelDelist,
                Action::ChannelBlockHandle,
                Action::ChannelRemove,
                Action::ChannelRestore,
            ],
            Self::Storage => &[Action::BlobDelete],
        }
    }

    /// The service that applies `action`.
    #[must_use]
    pub const fn of(action: Action) -> Self {
        match action {
            Action::Suspend | Action::Lift | Action::KeepSuspension => Self::Auth,
            Action::TakeBackName
            | Action::ReleaseName
            | Action::HideFromSearch
            | Action::ShowInSearch
            | Action::GroupDelist
            | Action::GroupBlockHandle
            | Action::GroupRestore => Self::Discovery,
            Action::ChannelDelist
            | Action::ChannelBlockHandle
            | Action::ChannelRemove
            | Action::ChannelRestore => Self::Channels,
            Action::BlobDelete => Self::Storage,
        }
    }

    /// A short lower-case name, for logs and configuration.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::Discovery => "discovery",
            Self::Channels => "channels",
            Self::Storage => "storage",
        }
    }
}

/// Why a statement was refused before anything was applied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApplyError {
    /// The moderator list does not verify under the pinned root.
    #[error("the moderator list does not verify")]
    BadList,
    /// The signed statement does not decode, or its assertion fails.
    #[error("the statement is malformed or its assertion fails")]
    BadStatement,
    /// The moderator is not on the list, was revoked when it signed, or the
    /// list is older than the one the statement names.
    #[error("the moderator is not authorised")]
    NotAuthorised,
    /// The action is not one this service applies.
    #[error("the action is not applied by this service")]
    WrongService,
    /// The reason and salt do not open the statement's commitment.
    #[error("the reason does not match the statement")]
    ReasonMismatch,
    /// The statement was signed more than [`MAX_STATEMENT_FUTURE_MS`] after
    /// the service's clock.
    #[error("the statement is dated in the future")]
    FutureStatement,
    /// The statement was signed more than [`MAX_STATEMENT_AGE_MS`] before the
    /// service's clock.
    #[error("the statement is too old")]
    StaleStatement,
    /// The moderator list is older than the newest one the service has seen.
    #[error("the moderator list is older than the newest seen")]
    StaleList,
}

impl ApplyError {
    /// The stable kebab code of the response body.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::BadList => "bad-list",
            Self::BadStatement => "bad-statement",
            Self::NotAuthorised => "not-authorised",
            Self::WrongService => "wrong-service",
            Self::ReasonMismatch => "reason-mismatch",
            Self::FutureStatement => "future-statement",
            Self::StaleStatement => "stale-statement",
            Self::StaleList => "stale-list",
        }
    }

    /// The HTTP status: 403 for [`Self::NotAuthorised`], 409 for
    /// [`Self::StaleList`], 400 otherwise.
    #[must_use]
    pub const fn status(&self) -> u16 {
        match self {
            Self::NotAuthorised => 403,
            Self::StaleList => 409,
            _ => 400,
        }
    }
}

/// A statement that passed every check that needs no service state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedStatement {
    /// The statement.
    pub statement: Statement,
    /// The signed statement it came in.
    pub signed: SignedStatement,
    /// The version of the list it verified under.
    pub list_version: u64,
    /// The signed statement's bytes, as handed in.
    pub signed_bytes: Vec<u8>,
    /// The reason its commitment opened to.
    pub reason: Reason,
    /// The salt of that commitment.
    pub reason_salt: [u8; 16],
}

impl CheckedStatement {
    /// The end of a suspension: `None` for no end and for every other action.
    #[must_use]
    pub const fn until_ms(&self) -> Option<u64> {
        match self.statement.until_ms {
            0 => None,
            u => Some(u),
        }
    }
}

/// What the service does with its list floor for an offered list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloorStep {
    /// The offered list is newer than the floor (or no floor is stored):
    /// raise the floor to it and keep it raised whatever happens next.
    Raise,
    /// The offered list is the floor.
    Keep,
}

/// Compare an offered list version with the stored floor: older is
/// [`ApplyError::StaleList`].
///
/// # Errors
///
/// [`ApplyError::StaleList`].
pub fn list_floor(stored: Option<u64>, offered: u64) -> Result<FloorStep, ApplyError> {
    match stored {
        Some(v) if offered < v => Err(ApplyError::StaleList),
        Some(v) if offered == v => Ok(FloorStep::Keep),
        _ => Ok(FloorStep::Raise),
    }
}

/// Whether `issued_at_ms` is within the freshness window around `now_ms`:
/// [`ApplyError::FutureStatement`] beyond [`MAX_STATEMENT_FUTURE_MS`] ahead,
/// [`ApplyError::StaleStatement`] beyond [`MAX_STATEMENT_AGE_MS`] behind. A
/// negative `now_ms` reads as zero.
///
/// # Errors
///
/// [`ApplyError::FutureStatement`], [`ApplyError::StaleStatement`].
pub fn check_fresh(issued_at_ms: u64, now_ms: i64) -> Result<(), ApplyError> {
    let now = u64::try_from(now_ms).unwrap_or(0);
    if issued_at_ms > now.saturating_add(MAX_STATEMENT_FUTURE_MS) {
        return Err(ApplyError::FutureStatement);
    }
    if now.saturating_sub(issued_at_ms) > MAX_STATEMENT_AGE_MS {
        return Err(ApplyError::StaleStatement);
    }
    Ok(())
}

/// The check, in order: the list verifies under `root_public`
/// ([`ApplyError::BadList`]); then [`check_with_list`]. Does not compare the
/// list with a stored floor; a service that keeps one verifies the list,
/// raises its floor ([`list_floor`]) and then calls [`check_with_list`].
///
/// # Errors
///
/// [`ApplyError`] except [`ApplyError::StaleList`].
pub fn check_for_service(
    list_bytes: &[u8],
    root_public: &[u8; 32],
    signed_statement: &[u8],
    reason: Reason,
    reason_salt: &[u8; 16],
    accepted: &[Action],
    now_ms: i64,
) -> Result<CheckedStatement, ApplyError> {
    let list = ModeratorList::verify(list_bytes, root_public).map_err(|_| ApplyError::BadList)?;
    check_with_list(
        &list,
        signed_statement,
        reason,
        reason_salt,
        accepted,
        now_ms,
    )
}

/// The check against a list the caller already verified under its pinned
/// root, in order: the signed statement decodes
/// ([`ApplyError::BadStatement`]); it verifies under `list`
/// ([`ApplyError::NotAuthorised`] for a moderator the list does not
/// authorise, [`ApplyError::BadStatement`] for any other failure, a failed
/// assertion included); it is fresh at `now_ms` ([`check_fresh`]); its
/// action is in `accepted`
/// ([`ApplyError::WrongService`]); `reason` and `reason_salt` open its
/// commitment ([`ApplyError::ReasonMismatch`]).
///
/// # Errors
///
/// [`ApplyError`] except [`ApplyError::BadList`] and [`ApplyError::StaleList`].
pub fn check_with_list(
    list: &ModeratorList,
    signed_statement: &[u8],
    reason: Reason,
    reason_salt: &[u8; 16],
    accepted: &[Action],
    now_ms: i64,
) -> Result<CheckedStatement, ApplyError> {
    let signed = SignedStatement::decode(signed_statement).map_err(|_| ApplyError::BadStatement)?;
    let statement = signed.verify(list).map_err(|e| match e {
        ModerationError::NotAuthorised => ApplyError::NotAuthorised,
        _ => ApplyError::BadStatement,
    })?;
    check_fresh(statement.issued_at_ms, now_ms)?;
    if !accepted.contains(&statement.action) {
        return Err(ApplyError::WrongService);
    }
    if !statement.reason_matches(reason, reason_salt) {
        return Err(ApplyError::ReasonMismatch);
    }
    Ok(CheckedStatement {
        statement,
        signed,
        list_version: list.list_version,
        signed_bytes: signed_statement.to_vec(),
        reason,
        reason_salt: *reason_salt,
    })
}

/// The body of `POST /internal/moderation/statement` at every applying
/// service, as the moderation service forwards a logged statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatementBody {
    /// Base64 of the signed statement.
    pub signed_statement_b64: String,
    /// Base64 of the root-signed moderator list the statement verifies
    /// under.
    pub moderator_list_b64: String,
    /// The statement's index in the moderation log.
    pub log_index: u64,
    /// The reason byte its commitment opens to.
    pub reason: u8,
    /// Base64 of the 16-byte salt of that commitment.
    pub reason_salt_b64: String,
}

/// Decode base64 in any of the four common spellings.
fn decode_b64(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    STANDARD
        .decode(text)
        .or_else(|_| STANDARD_NO_PAD.decode(text))
        .or_else(|_| URL_SAFE.decode(text))
        .or_else(|_| URL_SAFE_NO_PAD.decode(text))
        .ok()
}

impl StatementBody {
    /// The body for a statement, in standard padded base64.
    #[must_use]
    pub fn new(
        signed_statement: &[u8],
        moderator_list: &[u8],
        log_index: u64,
        reason: Reason,
        reason_salt: &[u8; 16],
    ) -> Self {
        Self {
            signed_statement_b64: STANDARD.encode(signed_statement),
            moderator_list_b64: STANDARD.encode(moderator_list),
            log_index,
            reason: reason.as_u8(),
            reason_salt_b64: STANDARD.encode(reason_salt),
        }
    }

    /// The body's moderator list, verified under `root_public`: a list that
    /// is not base64 or does not verify is [`ApplyError::BadList`]. The
    /// first step; a service compares the list with its floor
    /// ([`list_floor`]) and raises the floor before
    /// [`Self::check_against`].
    ///
    /// # Errors
    ///
    /// [`ApplyError::BadList`].
    pub fn verified_list(&self, root_public: &[u8; 32]) -> Result<ModeratorList, ApplyError> {
        let list_bytes = decode_b64(&self.moderator_list_b64).ok_or(ApplyError::BadList)?;
        ModeratorList::verify(&list_bytes, root_public).map_err(|_| ApplyError::BadList)
    }

    /// The checks after the list, against `list` (from
    /// [`Self::verified_list`]): a statement that is not base64 is
    /// [`ApplyError::BadStatement`], an unknown reason byte or a salt that is
    /// not 16 bytes [`ApplyError::ReasonMismatch`] (after every other check),
    /// and the rest as [`check_with_list`].
    ///
    /// # Errors
    ///
    /// [`ApplyError`] except [`ApplyError::BadList`] and
    /// [`ApplyError::StaleList`].
    pub fn check_against(
        &self,
        list: &ModeratorList,
        accepted: &[Action],
        now_ms: i64,
    ) -> Result<CheckedStatement, ApplyError> {
        let signed = decode_b64(&self.signed_statement_b64).unwrap_or_default();
        let salt = decode_b64(&self.reason_salt_b64).unwrap_or_default();
        check_raw(list, &signed, self.reason, &salt, accepted, now_ms)
    }

    /// [`Self::verified_list`] then [`Self::check_against`], with no floor in
    /// between. For a caller that keeps no floor (tests, tools).
    ///
    /// # Errors
    ///
    /// [`ApplyError`] except [`ApplyError::StaleList`].
    pub fn check(
        &self,
        root_public: &[u8; 32],
        accepted: &[Action],
        now_ms: i64,
    ) -> Result<CheckedStatement, ApplyError> {
        let list = self.verified_list(root_public)?;
        self.check_against(&list, accepted, now_ms)
    }
}

/// The body of `POST /internal/moderation/list` at every applying service:
/// the moderation service pushes its loaded list so each service's floor is
/// raised without waiting for a statement. `{"moderator_list_b64"}`, base64
/// in any of the four common spellings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListBody {
    /// Base64 of the root-signed moderator list.
    pub moderator_list_b64: String,
}

impl ListBody {
    /// The body for a list, in standard padded base64.
    #[must_use]
    pub fn new(moderator_list: &[u8]) -> Self {
        Self {
            moderator_list_b64: STANDARD.encode(moderator_list),
        }
    }

    /// The list, verified under `root_public`.
    ///
    /// # Errors
    ///
    /// [`ApplyError::BadList`].
    pub fn verify(&self, root_public: &[u8; 32]) -> Result<ModeratorList, ApplyError> {
        let list_bytes = decode_b64(&self.moderator_list_b64).ok_or(ApplyError::BadList)?;
        ModeratorList::verify(&list_bytes, root_public).map_err(|_| ApplyError::BadList)
    }
}

/// [`check_with_list`] over a reason byte and a salt as received: an unknown
/// reason byte or a salt that is not 16 bytes is
/// [`ApplyError::ReasonMismatch`], refused after every other check, like a
/// reason that does not open the commitment.
///
/// # Errors
///
/// [`ApplyError`] except [`ApplyError::BadList`] and [`ApplyError::StaleList`].
pub fn check_raw(
    list: &ModeratorList,
    signed_statement: &[u8],
    reason: u8,
    reason_salt: &[u8],
    accepted: &[Action],
    now_ms: i64,
) -> Result<CheckedStatement, ApplyError> {
    let reason = Reason::from_u8(reason).ok();
    let salt: Option<[u8; 16]> = reason_salt.try_into().ok();
    // Run the other checks under a stand-in and refuse whatever they pass.
    let checked = check_with_list(
        list,
        signed_statement,
        reason.unwrap_or(Reason::Spam),
        &salt.unwrap_or([0; 16]),
        accepted,
        now_ms,
    )?;
    if reason.is_none() || salt.is_none() {
        return Err(ApplyError::ReasonMismatch);
    }
    Ok(checked)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::moderators::tests::{entry, list, root_public, ROOT};
    use crate::statement::reason_commitment;
    use crate::target::Target;
    use crate::webauthn::tests::{keys, make};
    use proptest::prelude::*;

    /// A clock a second after the test statements were signed.
    const NOW: i64 = 6_000;

    fn signed_list() -> (Vec<u8>, ModeratorList) {
        let [ed, p] = keys();
        let mut l = list();
        l.entries = vec![entry(1, ed), entry(2, p)];
        (l.sign(&ROOT).unwrap(), l)
    }

    fn statement(action: Action, moderator: u8) -> Statement {
        let target = match action.target_kind() {
            crate::TargetKind::Account => Target::account(&[7; 32]).unwrap(),
            kind => Target::new(kind, &vec![9; kind.target_len()]).unwrap(),
        };
        Statement {
            statement_id: [1; 16],
            action,
            target,
            reason_commitment: reason_commitment(Reason::Spam, &[3; 16]),
            until_ms: 0,
            issued_at_ms: 5_000,
            moderator_id: [moderator; 16],
            list_version: 3,
        }
    }

    fn sign(s: &Statement, l: &ModeratorList) -> Vec<u8> {
        let [ed, _] = keys();
        SignedStatement {
            statement: s.encode(),
            assertion: make(l, &ed, &s.challenge(), 0x05),
        }
        .encode()
        .unwrap()
    }

    #[test]
    fn a_good_statement_passes_and_is_returned() {
        let (bytes, l) = signed_list();
        let s = statement(Action::Suspend, 1);
        let wire = sign(&s, &l);
        let got = check_for_service(
            &bytes,
            &root_public(),
            &wire,
            Reason::Spam,
            &[3; 16],
            ApplyingService::Auth.actions(),
            NOW,
        )
        .unwrap();
        assert_eq!(got.statement, s);
        assert_eq!(got.list_version, 3);
        assert_eq!(got.signed_bytes, wire);
        assert_eq!(got.until_ms(), None);
    }

    #[test]
    fn every_refusal_in_order() {
        let (bytes, l) = signed_list();
        let root = root_public();
        let s = statement(Action::Suspend, 1);
        let wire = sign(&s, &l);
        let auth = ApplyingService::Auth.actions();
        let run = |list: &[u8], st: &[u8], reason: Reason, salt: [u8; 16], acc: &[Action]| {
            check_for_service(list, &root, st, reason, &salt, acc, NOW).unwrap_err()
        };
        // A bad list is refused before anything else is looked at.
        assert_eq!(
            run(&[1, 2, 3], &[], Reason::Hate, [0; 16], &[]),
            ApplyError::BadList
        );
        assert_eq!(
            run(&bytes, &[1, 2, 3], Reason::Hate, [0; 16], &[]),
            ApplyError::BadStatement
        );
        // A moderator the list does not hold.
        let stranger = statement(Action::Suspend, 9);
        assert_eq!(
            run(&bytes, &sign(&stranger, &l), Reason::Spam, [3; 16], auth),
            ApplyError::NotAuthorised
        );
        // A failed assertion: the statement changed after signing.
        let mut tampered = SignedStatement::decode(&wire).unwrap();
        let mut other = s.clone();
        other.issued_at_ms += 1;
        tampered.statement = other.encode();
        assert_eq!(
            run(
                &bytes,
                &tampered.encode().unwrap(),
                Reason::Spam,
                [3; 16],
                auth
            ),
            ApplyError::BadStatement
        );
        assert_eq!(
            run(
                &bytes,
                &wire,
                Reason::Hate,
                [0; 16],
                ApplyingService::Storage.actions()
            ),
            ApplyError::WrongService
        );
        assert_eq!(
            run(&bytes, &wire, Reason::Hate, [3; 16], auth),
            ApplyError::ReasonMismatch
        );
        assert_eq!(
            run(&bytes, &wire, Reason::Spam, [4; 16], auth),
            ApplyError::ReasonMismatch
        );
    }

    #[test]
    fn codes_and_statuses_are_stable() {
        let all = [
            (ApplyError::BadList, "bad-list", 400),
            (ApplyError::BadStatement, "bad-statement", 400),
            (ApplyError::NotAuthorised, "not-authorised", 403),
            (ApplyError::WrongService, "wrong-service", 400),
            (ApplyError::ReasonMismatch, "reason-mismatch", 400),
            (ApplyError::FutureStatement, "future-statement", 400),
            (ApplyError::StaleStatement, "stale-statement", 400),
            (ApplyError::StaleList, "stale-list", 409),
        ];
        for (e, code, status) in all {
            assert_eq!(e.code(), code);
            assert_eq!(e.status(), status);
        }
    }

    #[test]
    fn every_action_has_exactly_one_service() {
        let services = [
            ApplyingService::Auth,
            ApplyingService::Discovery,
            ApplyingService::Channels,
            ApplyingService::Storage,
        ];
        for action in ALL_ACTIONS {
            let owners: Vec<_> = services
                .iter()
                .filter(|s| s.actions().contains(action))
                .collect();
            assert_eq!(owners, vec![&ApplyingService::of(*action)], "{action:?}");
        }
        let total: usize = services.iter().map(|s| s.actions().len()).sum();
        assert_eq!(total, ALL_ACTIONS.len());
        let bytes: Vec<u8> = ALL_ACTIONS.iter().map(|a| *a as u8).collect();
        assert_eq!(bytes, (1..=15).collect::<Vec<u8>>());
    }

    #[test]
    fn the_body_decodes_and_keeps_the_order() {
        let (bytes, l) = signed_list();
        let root = root_public();
        let s = statement(Action::Suspend, 1);
        let wire = sign(&s, &l);
        let auth = ApplyingService::Auth.actions();
        let body = StatementBody::new(&wire, &bytes, 7, Reason::Spam, &[3; 16]);
        let json = serde_json::to_string(&body).unwrap();
        let back: StatementBody = serde_json::from_str(&json).unwrap();
        assert_eq!(back, body);
        let got = back.check(&root, auth, NOW).unwrap();
        assert_eq!(got.reason, Reason::Spam);
        assert_eq!(got.reason_salt, [3; 16]);
        // url-safe unpadded is read too.
        let mut url = body.clone();
        url.signed_statement_b64 = URL_SAFE_NO_PAD.encode(&wire);
        assert!(url.check(&root, auth, NOW).is_ok());

        let with = |f: &dyn Fn(&mut StatementBody)| {
            let mut b = body.clone();
            f(&mut b);
            b.check(&root, auth, NOW).unwrap_err()
        };
        assert_eq!(
            with(&|b| b.moderator_list_b64 = "@@".into()),
            ApplyError::BadList
        );
        // A bad list wins over a bad statement and a bad reason.
        assert_eq!(
            with(&|b| {
                b.moderator_list_b64 = "AAAA".into();
                b.signed_statement_b64 = "@@".into();
                b.reason = 0;
            }),
            ApplyError::BadList
        );
        assert_eq!(
            with(&|b| b.signed_statement_b64 = "@@".into()),
            ApplyError::BadStatement
        );
        // An unreadable reason or salt is refused after the service check.
        assert_eq!(
            with(&|b| {
                b.reason = 0;
            }),
            ApplyError::ReasonMismatch
        );
        assert_eq!(
            with(&|b| b.reason_salt_b64 = STANDARD.encode([3; 15])),
            ApplyError::ReasonMismatch
        );
        let storage = ApplyingService::Storage.actions();
        let mut bad_reason = body.clone();
        bad_reason.reason = 0;
        assert_eq!(
            bad_reason.check(&root, storage, NOW).unwrap_err(),
            ApplyError::WrongService
        );
        // A salt of zeros that happens to open the commitment is still
        // refused when the reason byte is unknown.
        let mut zero = s.clone();
        zero.statement_id = [2; 16];
        zero.reason_commitment = reason_commitment(Reason::Spam, &[0; 16]);
        let zero_wire = sign(&zero, &l);
        let mut b = StatementBody::new(&zero_wire, &bytes, 1, Reason::Spam, &[0; 16]);
        assert!(b.check(&root, auth, NOW).is_ok());
        b.reason = 200;
        assert_eq!(
            b.check(&root, auth, NOW).unwrap_err(),
            ApplyError::ReasonMismatch
        );
    }

    #[test]
    fn a_statement_outside_its_window_is_refused_after_the_signature() {
        let (bytes, l) = signed_list();
        let root = root_public();
        let mut s = statement(Action::Suspend, 1);
        s.issued_at_ms = 10_000_000;
        let wire = sign(&s, &l);
        let auth = ApplyingService::Auth.actions();
        let at =
            |now: i64| check_for_service(&bytes, &root, &wire, Reason::Spam, &[3; 16], auth, now);
        let issued = i64::try_from(s.issued_at_ms).unwrap();
        let ahead = i64::try_from(MAX_STATEMENT_FUTURE_MS).unwrap();
        let age = i64::try_from(MAX_STATEMENT_AGE_MS).unwrap();
        assert!(at(issued).is_ok());
        assert!(at(issued - ahead).is_ok());
        assert_eq!(
            at(issued - ahead - 1).unwrap_err(),
            ApplyError::FutureStatement
        );
        assert!(at(issued + age).is_ok());
        assert_eq!(
            at(issued + age + 1).unwrap_err(),
            ApplyError::StaleStatement
        );
        // A clock before the epoch reads as zero.
        assert_eq!(at(-1).unwrap_err(), ApplyError::FutureStatement);
        // A stranger's statement is refused as not authorised, whatever its
        // age: freshness is checked after the signature.
        let stranger = statement(Action::Suspend, 9);
        assert_eq!(
            check_for_service(
                &bytes,
                &root,
                &sign(&stranger, &l),
                Reason::Spam,
                &[3; 16],
                auth,
                i64::MAX
            )
            .unwrap_err(),
            ApplyError::NotAuthorised
        );
        // A stale statement under the wrong service is stale, not wrong-service.
        assert_eq!(
            check_for_service(
                &bytes,
                &root,
                &wire,
                Reason::Spam,
                &[3; 16],
                ApplyingService::Storage.actions(),
                issued + age + 1
            )
            .unwrap_err(),
            ApplyError::StaleStatement
        );
    }

    #[test]
    fn the_floor_rises_and_refuses_an_older_list() {
        assert_eq!(list_floor(None, 1), Ok(FloorStep::Raise));
        assert_eq!(list_floor(Some(3), 4), Ok(FloorStep::Raise));
        assert_eq!(list_floor(Some(3), 3), Ok(FloorStep::Keep));
        assert_eq!(list_floor(Some(3), 2), Err(ApplyError::StaleList));
        let (bytes, l) = signed_list();
        let body = ListBody::new(&bytes);
        let json = serde_json::to_string(&body).unwrap();
        let back: ListBody = serde_json::from_str(&json).unwrap();
        assert_eq!(back.verify(&root_public()).unwrap(), l);
        assert_eq!(
            ListBody {
                moderator_list_b64: "@@".into()
            }
            .verify(&root_public())
            .unwrap_err(),
            ApplyError::BadList
        );
        assert_eq!(back.verify(&[1; 32]).unwrap_err(), ApplyError::BadList);
    }

    #[test]
    fn keep_suspension_is_an_auth_action() {
        assert_eq!(
            ApplyingService::of(Action::KeepSuspension),
            ApplyingService::Auth
        );
        let (bytes, l) = signed_list();
        let s = statement(Action::KeepSuspension, 1);
        let wire = sign(&s, &l);
        let got = check_for_service(
            &bytes,
            &root_public(),
            &wire,
            Reason::Spam,
            &[3; 16],
            ApplyingService::Auth.actions(),
            NOW,
        )
        .unwrap();
        assert_eq!(got.statement.action, Action::KeepSuspension);
        assert_eq!(got.until_ms(), None);
    }

    proptest! {
        #[test]
        fn nothing_unsigned_passes(bytes in proptest::collection::vec(any::<u8>(), 0..400)) {
            let (list_bytes, _) = signed_list();
            prop_assert!(check_for_service(
                &list_bytes,
                &root_public(),
                &bytes,
                Reason::Spam,
                &[3; 16],
                ALL_ACTIONS,
                NOW,
            )
            .is_err());
        }
    }
}
