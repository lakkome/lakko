// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! "Who can find me" — the setting and the reach-key state, as envelopes
//! the owner's own devices exchange
//! ([ADR-0060](../../../docs/adr/0060-scoped-discovery.md) §7, UX spec
//! `docs/ux/who-can-find-me.md` sections 1 and 3).
//!
//! ## The setting
//!
//! Three positions: **Nobody**, **Some people** (made of two parts that
//! combine: contacts of my contacts; members of all, or of chosen, groups and
//! channels) and **Everyone**. [`VisibilitySetting`] carries the position,
//! the parts, the all/chosen form, the chosen groups and channels, and the
//! "last parts" memory the UX restores when a person comes back to Some
//! people. The transition rules that turn taps into a setting live in
//! `lakko_ui_rules::visibility`; this module owns the shape and its one
//! validity rule.
//!
//! ### The canonical-form rule
//!
//! [`VisibilitySetting::validate`] runs on compose AND decode:
//!
//! * Some people requires at least one part on (Some people with nothing on
//!   means Nobody, and the UI says so rather than storing the contradiction);
//! * Nobody and Everyone carry no parts — the memory is in `last_*`;
//! * the chosen list is at most [`MAX_CHOSEN_SCOPES`] entries, has no
//!   duplicate, and every id is 1 to [`MAX_SCOPE_ID_LEN`] bytes.
//!
//! The chosen list is kept whatever the form, so switching from "only the
//! ones I choose" to "all" and back does not lose the person's choices.
//!
//! ## The reach state
//!
//! [`ReachState`] carries every reach key version counter
//! ([`crate::reach`]) so all of a person's devices derive the same keys. One
//! entry per `(class, scope)`.
//!
//! ## Order
//!
//! Both envelopes are ordered by a strictly increasing stamp
//! ([`visibility_is_newer`], [`reach_state_is_newer`]); a re-send carries its
//! original stamp and is not an update (the ADR-0044 lesson).
//!
//! ## Wire (v1)
//!
//! ```text
//! visibility  = "lakko/visibility/v1\x00" || version(1)=0x01 || updated_at_ms(8, BE)
//!               || mode(1) || flags(1) || groups(1) || last_groups(1)
//!               || chosen_count(2, BE) || chosen_count x [ kind(1) || id_len(1) || id ]
//!   flags: bit 0 contacts_of_contacts, bit 1 last_contacts_of_contacts, others 0
//!   groups / last_groups: 0 none, 1 all, 2 chosen
//!
//! reach_state = "lakko/reach-state/v1\x00" || version(1)=0x01 || updated_at_ms(8, BE)
//!               || count(2, BE) || count x [ class(1) || scope_len(1) || scope || version(4, BE) ]
//! ```

use core::result::Result as CoreResult;

use thiserror::Error;

use crate::reach::{ReachClass, ReachError};

/// Domain separator of the setting envelope. Frozen.
pub const VISIBILITY_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/visibility/v1\x00";

/// Setting schema version.
pub const VISIBILITY_VERSION_V1: u8 = 0x01;

/// Domain separator of the reach-state envelope. Frozen.
pub const REACH_STATE_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/reach-state/v1\x00";

/// Reach-state schema version.
pub const REACH_STATE_VERSION_V1: u8 = 0x01;

/// Longest group or channel id in the chosen list.
pub const MAX_SCOPE_ID_LEN: usize = 64;

/// Most groups and channels the chosen list holds.
pub const MAX_CHOSEN_SCOPES: usize = 1024;

/// Most entries a reach state holds. Chosen, not measured.
pub const MAX_REACH_STATE_ENTRIES: usize = 4096;

const FLAG_CONTACTS_OF_CONTACTS: u8 = 0b01;
const FLAG_LAST_CONTACTS_OF_CONTACTS: u8 = 0b10;

/// The three positions (UX section 1). Wire bytes frozen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VisibilityMode {
    /// No one finds the person by username or name.
    Nobody,
    /// The parts that are on.
    Some,
    /// Anyone finds the person.
    Everyone,
}

impl VisibilityMode {
    /// The frozen wire byte.
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        match self {
            Self::Nobody => 0,
            Self::Some => 1,
            Self::Everyone => 2,
        }
    }

    /// Parse a wire byte, fail-closed.
    ///
    /// # Errors
    ///
    /// [`VisibilityError::UnknownMode`].
    pub const fn from_byte(byte: u8) -> CoreResult<Self, VisibilityError> {
        match byte {
            0 => Ok(Self::Nobody),
            1 => Ok(Self::Some),
            2 => Ok(Self::Everyone),
            other => Err(VisibilityError::UnknownMode(other)),
        }
    }
}

/// All groups and channels, or only the chosen ones (UX 2.2.1 / 2.2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupsForm {
    /// Every group and channel the person is in.
    All,
    /// Only the ones in the chosen list.
    Chosen,
}

impl GroupsForm {
    /// The frozen wire byte.
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        match self {
            Self::All => 1,
            Self::Chosen => 2,
        }
    }
}

fn groups_to_byte(groups: Option<GroupsForm>) -> u8 {
    groups.map_or(0, GroupsForm::to_byte)
}

const fn groups_from_byte(byte: u8) -> CoreResult<Option<GroupsForm>, VisibilityError> {
    match byte {
        0 => Ok(None),
        1 => Ok(Some(GroupsForm::All)),
        2 => Ok(Some(GroupsForm::Chosen)),
        other => Err(VisibilityError::UnknownGroupsForm(other)),
    }
}

/// Group or channel. Wire bytes frozen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ScopeKind {
    /// A group (MLS group id).
    Group,
    /// A channel (channel id).
    Channel,
}

impl ScopeKind {
    /// The frozen wire byte.
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        match self {
            Self::Group => 1,
            Self::Channel => 2,
        }
    }

    /// Parse a wire byte, fail-closed.
    ///
    /// # Errors
    ///
    /// [`VisibilityError::UnknownScopeKind`].
    pub const fn from_byte(byte: u8) -> CoreResult<Self, VisibilityError> {
        match byte {
            1 => Ok(Self::Group),
            2 => Ok(Self::Channel),
            other => Err(VisibilityError::UnknownScopeKind(other)),
        }
    }
}

/// One group or channel.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeId {
    /// Group or channel.
    pub kind: ScopeKind,
    /// The id, 1 to [`MAX_SCOPE_ID_LEN`] bytes.
    pub id: Vec<u8>,
}

impl ScopeId {
    /// Check the id length.
    ///
    /// # Errors
    ///
    /// [`VisibilityError::ScopeIdLength`].
    pub fn validate(&self) -> CoreResult<(), VisibilityError> {
        if self.id.is_empty() || self.id.len() > MAX_SCOPE_ID_LEN {
            return Err(VisibilityError::ScopeIdLength { len: self.id.len() });
        }
        Ok(())
    }
}

/// The "who can find me" setting (UX sections 1 and 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibilitySetting {
    /// The position.
    pub mode: VisibilityMode,
    /// Part 2.1: contacts of my contacts. Only while `mode` is Some.
    pub contacts_of_contacts: bool,
    /// Part 2.2, and its form; `None` when off. Only while `mode` is Some.
    pub groups: Option<GroupsForm>,
    /// The groups and channels switched on under "only the ones I choose".
    /// Kept whatever the form.
    pub chosen: Vec<ScopeId>,
    /// Memory: whether part 2.1 was on when the person last left Some.
    pub last_contacts_of_contacts: bool,
    /// Memory: part 2.2's form when the person last left Some.
    pub last_groups: Option<GroupsForm>,
    /// Setter-clock UNIX ms. Orders settings via [`visibility_is_newer`].
    pub updated_at_ms: u64,
}

impl VisibilitySetting {
    /// Default for a new account: Some people, with contacts of contacts and
    /// all groups and channels on (UX section 1); the memory the same.
    #[must_use]
    pub const fn default_for_new_account() -> Self {
        Self {
            mode: VisibilityMode::Some,
            contacts_of_contacts: true,
            groups: Some(GroupsForm::All),
            chosen: Vec::new(),
            last_contacts_of_contacts: true,
            last_groups: Some(GroupsForm::All),
            updated_at_ms: 0,
        }
    }

    /// The canonical-form rule (module docs). Run by compose and decode.
    ///
    /// # Errors
    ///
    /// [`VisibilityError::SomeWithNoPart`], [`VisibilityError::PartsOutsideSome`],
    /// [`VisibilityError::TooManyChosen`], [`VisibilityError::DuplicateScope`],
    /// [`VisibilityError::ScopeIdLength`].
    pub fn validate(&self) -> CoreResult<(), VisibilityError> {
        match self.mode {
            VisibilityMode::Some => {
                if !self.contacts_of_contacts && self.groups.is_none() {
                    return Err(VisibilityError::SomeWithNoPart);
                }
            },
            VisibilityMode::Nobody | VisibilityMode::Everyone => {
                if self.contacts_of_contacts || self.groups.is_some() {
                    return Err(VisibilityError::PartsOutsideSome);
                }
            },
        }
        if self.chosen.len() > MAX_CHOSEN_SCOPES {
            return Err(VisibilityError::TooManyChosen {
                count: self.chosen.len(),
            });
        }
        let mut seen = std::collections::BTreeSet::new();
        for scope in &self.chosen {
            scope.validate()?;
            if !seen.insert(scope) {
                return Err(VisibilityError::DuplicateScope);
            }
        }
        Ok(())
    }

    /// Whether the members of `scope` can find this person: Everyone, or
    /// Some people with part 2.2 on and either all groups or this one chosen.
    ///
    /// This is the permission, not a delivery decision: under Everyone the
    /// public record already covers the group, and whether a client still
    /// sends a grant there is the client's call.
    #[must_use]
    pub fn allows_scope(&self, scope: &ScopeId) -> bool {
        match self.mode {
            VisibilityMode::Nobody => false,
            VisibilityMode::Everyone => true,
            VisibilityMode::Some => match self.groups {
                None => false,
                Some(GroupsForm::All) => true,
                Some(GroupsForm::Chosen) => self.chosen.contains(scope),
            },
        }
    }

    /// Whether contacts of contacts can find this person: Everyone, or Some
    /// people with part 2.1 on. The same caveat as [`Self::allows_scope`].
    #[must_use]
    pub const fn allows_contacts_of_contacts(&self) -> bool {
        match self.mode {
            VisibilityMode::Nobody => false,
            VisibilityMode::Everyone => true,
            VisibilityMode::Some => self.contacts_of_contacts,
        }
    }
}

/// `true` when a setting stamped `incoming_updated_at_ms` must replace the
/// stored one. Strictly greater wins; equal is not newer.
#[must_use]
pub const fn visibility_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    incoming_updated_at_ms > stored_updated_at_ms
}

/// Compose the setting envelope, sent over the owner's DeviceGroup.
///
/// # Errors
///
/// Exactly what [`VisibilitySetting::validate`] returns.
pub fn compose_visibility(setting: &VisibilitySetting) -> CoreResult<Vec<u8>, VisibilityError> {
    setting.validate()?;
    let mut out = Vec::new();
    out.extend_from_slice(VISIBILITY_ENVELOPE_DOMAIN_V1);
    out.push(VISIBILITY_VERSION_V1);
    out.extend_from_slice(&setting.updated_at_ms.to_be_bytes());
    out.push(setting.mode.to_byte());
    let mut flags = 0u8;
    if setting.contacts_of_contacts {
        flags |= FLAG_CONTACTS_OF_CONTACTS;
    }
    if setting.last_contacts_of_contacts {
        flags |= FLAG_LAST_CONTACTS_OF_CONTACTS;
    }
    out.push(flags);
    out.push(groups_to_byte(setting.groups));
    out.push(groups_to_byte(setting.last_groups));
    let count =
        u16::try_from(setting.chosen.len()).map_err(|_| VisibilityError::TooManyChosen {
            count: setting.chosen.len(),
        })?;
    out.extend_from_slice(&count.to_be_bytes());
    for scope in &setting.chosen {
        out.push(scope.kind.to_byte());
        out.push(
            u8::try_from(scope.id.len()).map_err(|_| VisibilityError::ScopeIdLength {
                len: scope.id.len(),
            })?,
        );
        out.extend_from_slice(&scope.id);
    }
    Ok(out)
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher.
///
/// # Errors
///
/// Framing errors, unknown enum bytes, [`VisibilityError::ReservedFlags`] and
/// every error of [`VisibilitySetting::validate`].
pub fn try_unwrap_visibility(
    plaintext: &[u8],
) -> CoreResult<Option<VisibilitySetting>, VisibilityError> {
    let Some(payload) = plaintext.strip_prefix(VISIBILITY_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let mut cur = payload;
    let version = take_u8(&mut cur)?;
    if version != VISIBILITY_VERSION_V1 {
        return Err(VisibilityError::UnknownVersion(version));
    }
    let updated_at_ms = u64::from_be_bytes(take_array::<8>(&mut cur)?);
    let mode = VisibilityMode::from_byte(take_u8(&mut cur)?)?;
    let flags = take_u8(&mut cur)?;
    if flags & !(FLAG_CONTACTS_OF_CONTACTS | FLAG_LAST_CONTACTS_OF_CONTACTS) != 0 {
        return Err(VisibilityError::ReservedFlags(flags));
    }
    let groups = groups_from_byte(take_u8(&mut cur)?)?;
    let last_groups = groups_from_byte(take_u8(&mut cur)?)?;
    let count = usize::from(u16::from_be_bytes(take_array::<2>(&mut cur)?));
    if count > MAX_CHOSEN_SCOPES {
        return Err(VisibilityError::TooManyChosen { count });
    }
    let mut chosen = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = ScopeKind::from_byte(take_u8(&mut cur)?)?;
        let len = usize::from(take_u8(&mut cur)?);
        chosen.push(ScopeId {
            kind,
            id: take(&mut cur, len)?.to_vec(),
        });
    }
    if !cur.is_empty() {
        return Err(VisibilityError::TrailingBytes { count: cur.len() });
    }
    let setting = VisibilitySetting {
        mode,
        contacts_of_contacts: flags & FLAG_CONTACTS_OF_CONTACTS != 0,
        groups,
        chosen,
        last_contacts_of_contacts: flags & FLAG_LAST_CONTACTS_OF_CONTACTS != 0,
        last_groups,
        updated_at_ms,
    };
    setting.validate()?;
    Ok(Some(setting))
}

/// Strict standalone decoder for the setting.
///
/// # Errors
///
/// [`VisibilityError::WrongDomain`] and every error of
/// [`try_unwrap_visibility`].
pub fn decode_visibility(bytes: &[u8]) -> CoreResult<VisibilitySetting, VisibilityError> {
    try_unwrap_visibility(bytes)?.ok_or(VisibilityError::WrongDomain)
}

// =============================================================================
// Reach state
// =============================================================================

/// One reach key's version counter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachStateEntry {
    /// The class.
    pub class: ReachClass,
    /// The scope, shaped per [`ReachClass::check_scope`].
    pub scope_id: Vec<u8>,
    /// The current key version.
    pub version: u32,
}

/// Every reach key version counter the owner's devices share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachState {
    /// At most [`MAX_REACH_STATE_ENTRIES`], one per `(class, scope_id)`.
    pub entries: Vec<ReachStateEntry>,
    /// Setter-clock UNIX ms. Orders states via [`reach_state_is_newer`].
    pub updated_at_ms: u64,
}

impl ReachState {
    /// Check every bound.
    ///
    /// # Errors
    ///
    /// [`VisibilityError::TooManyEntries`], [`VisibilityError::DuplicateEntry`],
    /// [`VisibilityError::Reach`].
    pub fn validate(&self) -> CoreResult<(), VisibilityError> {
        if self.entries.len() > MAX_REACH_STATE_ENTRIES {
            return Err(VisibilityError::TooManyEntries {
                count: self.entries.len(),
            });
        }
        let mut seen = std::collections::BTreeSet::new();
        for entry in &self.entries {
            entry
                .class
                .check_scope(&entry.scope_id)
                .map_err(VisibilityError::Reach)?;
            if !seen.insert((entry.class, entry.scope_id.as_slice())) {
                return Err(VisibilityError::DuplicateEntry);
            }
        }
        Ok(())
    }

    /// The version of `(class, scope_id)`, if the state holds it.
    #[must_use]
    pub fn version_of(&self, class: ReachClass, scope_id: &[u8]) -> Option<u32> {
        self.entries
            .iter()
            .find(|e| e.class == class && e.scope_id == scope_id)
            .map(|e| e.version)
    }
}

/// `true` when a reach state stamped `incoming_updated_at_ms` must replace
/// the stored one. Strictly greater wins; equal is not newer.
#[must_use]
pub const fn reach_state_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    incoming_updated_at_ms > stored_updated_at_ms
}

/// Compose the reach-state envelope.
///
/// # Errors
///
/// Exactly what [`ReachState::validate`] returns.
pub fn compose_reach_state(state: &ReachState) -> CoreResult<Vec<u8>, VisibilityError> {
    state.validate()?;
    let mut out = Vec::new();
    out.extend_from_slice(REACH_STATE_ENVELOPE_DOMAIN_V1);
    out.push(REACH_STATE_VERSION_V1);
    out.extend_from_slice(&state.updated_at_ms.to_be_bytes());
    let count =
        u16::try_from(state.entries.len()).map_err(|_| VisibilityError::TooManyEntries {
            count: state.entries.len(),
        })?;
    out.extend_from_slice(&count.to_be_bytes());
    for entry in &state.entries {
        out.push(entry.class.to_byte());
        out.push(u8::try_from(entry.scope_id.len()).map_err(|_| {
            VisibilityError::Reach(ReachError::ScopeIdTooLong {
                len: entry.scope_id.len(),
            })
        })?);
        out.extend_from_slice(&entry.scope_id);
        out.extend_from_slice(&entry.version.to_be_bytes());
    }
    Ok(out)
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher.
///
/// # Errors
///
/// Framing errors and every error of [`ReachState::validate`].
pub fn try_unwrap_reach_state(plaintext: &[u8]) -> CoreResult<Option<ReachState>, VisibilityError> {
    let Some(payload) = plaintext.strip_prefix(REACH_STATE_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let mut cur = payload;
    let version = take_u8(&mut cur)?;
    if version != REACH_STATE_VERSION_V1 {
        return Err(VisibilityError::UnknownVersion(version));
    }
    let updated_at_ms = u64::from_be_bytes(take_array::<8>(&mut cur)?);
    let count = usize::from(u16::from_be_bytes(take_array::<2>(&mut cur)?));
    if count > MAX_REACH_STATE_ENTRIES {
        return Err(VisibilityError::TooManyEntries { count });
    }
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let class = ReachClass::from_byte(take_u8(&mut cur)?).map_err(VisibilityError::Reach)?;
        let len = usize::from(take_u8(&mut cur)?);
        let scope_id = take(&mut cur, len)?.to_vec();
        let version = u32::from_be_bytes(take_array::<4>(&mut cur)?);
        entries.push(ReachStateEntry {
            class,
            scope_id,
            version,
        });
    }
    if !cur.is_empty() {
        return Err(VisibilityError::TrailingBytes { count: cur.len() });
    }
    let state = ReachState {
        entries,
        updated_at_ms,
    };
    state.validate()?;
    Ok(Some(state))
}

/// Strict standalone decoder for the reach state.
///
/// # Errors
///
/// [`VisibilityError::WrongDomain`] and every error of
/// [`try_unwrap_reach_state`].
pub fn decode_reach_state(bytes: &[u8]) -> CoreResult<ReachState, VisibilityError> {
    try_unwrap_reach_state(bytes)?.ok_or(VisibilityError::WrongDomain)
}

// =============================================================================
// Errors and cursor helpers
// =============================================================================

/// Why a setting or reach-state envelope was refused. A reason code; never
/// a scope id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum VisibilityError {
    /// The plaintext does not carry the expected domain separator (strict
    /// decoders only).
    #[error("plaintext does not carry the visibility domain separator")]
    WrongDomain,
    /// The version byte is not the one accepted.
    #[error("visibility envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The bytes end before a declared field.
    #[error("visibility envelope is truncated")]
    Truncated,
    /// Bytes remain after the last field.
    #[error("visibility envelope has {count} trailing bytes")]
    TrailingBytes {
        /// How many.
        count: usize,
    },
    /// The mode byte is not a position.
    #[error("unknown visibility mode {0:#04x}")]
    UnknownMode(u8),
    /// A groups byte is not a form.
    #[error("unknown groups form {0:#04x}")]
    UnknownGroupsForm(u8),
    /// A scope-kind byte is neither group nor channel.
    #[error("unknown scope kind {0:#04x}")]
    UnknownScopeKind(u8),
    /// A reserved flag bit is set.
    #[error("visibility flags {0:#04x} set a reserved bit")]
    ReservedFlags(u8),
    /// Some people with no part on.
    #[error("'some people' with no part on")]
    SomeWithNoPart,
    /// A part on outside Some people.
    #[error("a part is on outside 'some people'")]
    PartsOutsideSome,
    /// More chosen scopes than [`MAX_CHOSEN_SCOPES`].
    #[error("{count} chosen groups and channels, at most {MAX_CHOSEN_SCOPES} allowed")]
    TooManyChosen {
        /// The count.
        count: usize,
    },
    /// A scope chosen twice.
    #[error("a group or channel is chosen twice")]
    DuplicateScope,
    /// A scope id outside 1 to [`MAX_SCOPE_ID_LEN`] bytes.
    #[error("scope id is {len} bytes, expected 1 to {MAX_SCOPE_ID_LEN}")]
    ScopeIdLength {
        /// The length.
        len: usize,
    },
    /// More reach-state entries than [`MAX_REACH_STATE_ENTRIES`].
    #[error("{count} reach-state entries, at most {MAX_REACH_STATE_ENTRIES} allowed")]
    TooManyEntries {
        /// The count.
        count: usize,
    },
    /// Two reach-state entries for one `(class, scope)`.
    #[error("a reach key is listed twice")]
    DuplicateEntry,
    /// A reach class or scope violates its rule.
    #[error("invalid reach entry: {0}")]
    Reach(ReachError),
}

impl VisibilityError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub fn code(self) -> String {
        match self {
            Self::WrongDomain => "wrongDomain".to_string(),
            Self::UnknownVersion(_) => "unknownVersion".to_string(),
            Self::Truncated => "truncated".to_string(),
            Self::TrailingBytes { .. } => "trailingBytes".to_string(),
            Self::UnknownMode(_) => "unknownMode".to_string(),
            Self::UnknownGroupsForm(_) => "unknownGroupsForm".to_string(),
            Self::UnknownScopeKind(_) => "unknownScopeKind".to_string(),
            Self::ReservedFlags(_) => "reservedFlags".to_string(),
            Self::SomeWithNoPart => "someWithNoPart".to_string(),
            Self::PartsOutsideSome => "partsOutsideSome".to_string(),
            Self::TooManyChosen { .. } => "tooManyChosen".to_string(),
            Self::DuplicateScope => "duplicateScope".to_string(),
            Self::ScopeIdLength { .. } => "scopeIdLength".to_string(),
            Self::TooManyEntries { .. } => "tooManyEntries".to_string(),
            Self::DuplicateEntry => "duplicateEntry".to_string(),
            Self::Reach(e) => format!("reach.{}", e.code()),
        }
    }
}

fn take_u8(cur: &mut &[u8]) -> CoreResult<u8, VisibilityError> {
    let (&first, rest) = cur.split_first().ok_or(VisibilityError::Truncated)?;
    *cur = rest;
    Ok(first)
}

fn take<'a>(cur: &mut &'a [u8], n: usize) -> CoreResult<&'a [u8], VisibilityError> {
    if cur.len() < n {
        return Err(VisibilityError::Truncated);
    }
    let (head, rest) = cur.split_at(n);
    *cur = rest;
    Ok(head)
}

fn take_array<const N: usize>(cur: &mut &[u8]) -> CoreResult<[u8; N], VisibilityError> {
    take(cur, N)?
        .try_into()
        .map_err(|_| VisibilityError::Truncated)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reach::MAX_REACH_SCOPE_ID_LEN;

    fn scope(kind: ScopeKind, byte: u8, len: usize) -> ScopeId {
        ScopeId {
            kind,
            id: vec![byte; len],
        }
    }

    /// Every combination of the small fields, valid or not.
    fn all_shapes() -> Vec<VisibilitySetting> {
        let mut out = Vec::new();
        for mode in [
            VisibilityMode::Nobody,
            VisibilityMode::Some,
            VisibilityMode::Everyone,
        ] {
            for cofc in [false, true] {
                for groups in [None, Some(GroupsForm::All), Some(GroupsForm::Chosen)] {
                    for last_cofc in [false, true] {
                        for last_groups in [None, Some(GroupsForm::All), Some(GroupsForm::Chosen)] {
                            out.push(VisibilitySetting {
                                mode,
                                contacts_of_contacts: cofc,
                                groups,
                                chosen: vec![
                                    scope(ScopeKind::Group, 1, 32),
                                    scope(ScopeKind::Channel, 2, 16),
                                ],
                                last_contacts_of_contacts: last_cofc,
                                last_groups,
                                updated_at_ms: 42,
                            });
                        }
                    }
                }
            }
        }
        out
    }

    fn expected_valid(s: &VisibilitySetting) -> bool {
        match s.mode {
            VisibilityMode::Some => s.contacts_of_contacts || s.groups.is_some(),
            _ => !s.contacts_of_contacts && s.groups.is_none(),
        }
    }

    #[test]
    fn every_valid_shape_round_trips_and_every_invalid_one_is_refused() {
        let mut valid = 0;
        for s in all_shapes() {
            if expected_valid(&s) {
                valid += 1;
                let wire = compose_visibility(&s).unwrap();
                assert_eq!(decode_visibility(&wire).unwrap(), s);
            } else {
                let why = compose_visibility(&s).unwrap_err();
                assert!(matches!(
                    why,
                    VisibilityError::SomeWithNoPart | VisibilityError::PartsOutsideSome
                ));
                assert_eq!(decode_visibility(&raw_visibility(&s)), Err(why), "{s:?}");
            }
        }
        // Some: 5 valid part combinations; Nobody / Everyone: 1 each; times
        // 2 x 3 memory combinations.
        assert_eq!(valid, (5 + 1 + 1) * 6);
    }

    /// Encode past `validate`.
    fn raw_visibility(s: &VisibilitySetting) -> Vec<u8> {
        let mut out = VISIBILITY_ENVELOPE_DOMAIN_V1.to_vec();
        out.push(VISIBILITY_VERSION_V1);
        out.extend_from_slice(&s.updated_at_ms.to_be_bytes());
        out.push(s.mode.to_byte());
        out.push(u8::from(s.contacts_of_contacts) | (u8::from(s.last_contacts_of_contacts) << 1));
        out.push(groups_to_byte(s.groups));
        out.push(groups_to_byte(s.last_groups));
        out.extend_from_slice(&u16::try_from(s.chosen.len()).unwrap().to_be_bytes());
        for c in &s.chosen {
            out.push(c.kind.to_byte());
            out.push(u8::try_from(c.id.len()).unwrap());
            out.extend_from_slice(&c.id);
        }
        out
    }

    #[test]
    fn the_default_is_some_with_both_parts_and_is_valid() {
        let d = VisibilitySetting::default_for_new_account();
        d.validate().unwrap();
        assert_eq!(d.mode, VisibilityMode::Some);
        assert!(d.contacts_of_contacts);
        assert_eq!(d.groups, Some(GroupsForm::All));
        assert!(d.allows_contacts_of_contacts());
        assert!(d.allows_scope(&scope(ScopeKind::Group, 9, 8)));
    }

    #[test]
    fn allows_follows_the_setting() {
        let a = scope(ScopeKind::Group, 1, 32);
        let b = scope(ScopeKind::Group, 3, 32);
        let mut s = VisibilitySetting::default_for_new_account();
        s.groups = Some(GroupsForm::Chosen);
        s.chosen = vec![a.clone()];
        assert!(s.allows_scope(&a));
        assert!(!s.allows_scope(&b));
        s.groups = None;
        assert!(!s.allows_scope(&a));
        s.mode = VisibilityMode::Nobody;
        s.contacts_of_contacts = false;
        assert!(!s.allows_scope(&a) && !s.allows_contacts_of_contacts());
        s.mode = VisibilityMode::Everyone;
        assert!(s.allows_scope(&b) && s.allows_contacts_of_contacts());
    }

    #[test]
    fn chosen_bounds_are_refused_by_compose_and_decode_alike() {
        let base = VisibilitySetting::default_for_new_account();
        let cases = [
            (
                vec![scope(ScopeKind::Group, 1, 0)],
                VisibilityError::ScopeIdLength { len: 0 },
            ),
            (
                vec![scope(ScopeKind::Group, 1, MAX_SCOPE_ID_LEN + 1)],
                VisibilityError::ScopeIdLength {
                    len: MAX_SCOPE_ID_LEN + 1,
                },
            ),
            (
                vec![scope(ScopeKind::Group, 1, 8), scope(ScopeKind::Group, 1, 8)],
                VisibilityError::DuplicateScope,
            ),
            (
                (0..=MAX_CHOSEN_SCOPES)
                    .map(|i| ScopeId {
                        kind: ScopeKind::Group,
                        id: u32::try_from(i).unwrap().to_be_bytes().to_vec(),
                    })
                    .collect(),
                VisibilityError::TooManyChosen {
                    count: MAX_CHOSEN_SCOPES + 1,
                },
            ),
        ];
        for (chosen, why) in cases {
            let mut s = base.clone();
            s.chosen = chosen;
            assert_eq!(compose_visibility(&s), Err(why));
            assert_eq!(decode_visibility(&raw_visibility(&s)), Err(why));
        }
        // A group and a channel with the same id are distinct scopes.
        let mut s = base.clone();
        s.chosen = vec![
            scope(ScopeKind::Group, 1, 8),
            scope(ScopeKind::Channel, 1, 8),
        ];
        compose_visibility(&s).unwrap();
        // The full list, with maximal ids, fits.
        let mut full = base;
        full.chosen = (0..MAX_CHOSEN_SCOPES)
            .map(|i| {
                let mut id = vec![0; MAX_SCOPE_ID_LEN];
                id[..4].copy_from_slice(&u32::try_from(i).unwrap().to_be_bytes());
                ScopeId {
                    kind: ScopeKind::Channel,
                    id,
                }
            })
            .collect();
        let wire = compose_visibility(&full).unwrap();
        assert_eq!(decode_visibility(&wire).unwrap(), full);
    }

    #[test]
    fn visibility_framing_is_fail_closed() {
        let wire =
            compose_visibility(&all_shapes().into_iter().find(expected_valid).unwrap()).unwrap();
        let dom = VISIBILITY_ENVELOPE_DOMAIN_V1.len();
        for cut in dom..wire.len() {
            assert_eq!(
                try_unwrap_visibility(&wire[..cut]),
                Err(VisibilityError::Truncated),
                "cut at {cut}"
            );
        }
        let mut long = wire.clone();
        long.push(0);
        assert_eq!(
            decode_visibility(&long),
            Err(VisibilityError::TrailingBytes { count: 1 })
        );
        let patch = |at: usize, byte: u8| {
            let mut out = wire.clone();
            out[at] = byte;
            out
        };
        assert_eq!(
            decode_visibility(&patch(dom, 2)),
            Err(VisibilityError::UnknownVersion(2))
        );
        assert_eq!(
            decode_visibility(&patch(dom + 9, 3)),
            Err(VisibilityError::UnknownMode(3))
        );
        assert!(matches!(
            decode_visibility(&patch(dom + 10, wire[dom + 10] | 0b100)),
            Err(VisibilityError::ReservedFlags(_))
        ));
        assert_eq!(
            decode_visibility(&patch(dom + 11, 3)),
            Err(VisibilityError::UnknownGroupsForm(3))
        );
        assert_eq!(
            decode_visibility(&patch(dom + 12, 9)),
            Err(VisibilityError::UnknownGroupsForm(9))
        );
        assert_eq!(
            decode_visibility(&patch(dom + 15, 0)),
            Err(VisibilityError::UnknownScopeKind(0))
        );
        let mut huge = wire;
        huge[dom + 13..dom + 15].copy_from_slice(&u16::MAX.to_be_bytes());
        assert_eq!(
            decode_visibility(&huge),
            Err(VisibilityError::TooManyChosen {
                count: usize::from(u16::MAX)
            })
        );
        assert_eq!(try_unwrap_visibility(b"hello"), Ok(None));
        assert_eq!(
            decode_visibility(b"hello"),
            Err(VisibilityError::WrongDomain)
        );
    }

    #[test]
    fn visibility_layout_is_pinned() {
        let mut s = VisibilitySetting::default_for_new_account();
        s.updated_at_ms = 0x0102_0304_0506_0708;
        s.chosen = vec![scope(ScopeKind::Channel, 0xAA, 2)];
        let wire = compose_visibility(&s).unwrap();
        let mut expected = b"lakko/visibility/v1\x00".to_vec();
        expected.extend_from_slice(&[
            1, 1, 2, 3, 4, 5, 6, 7, 8, 1, 0b11, 1, 1, 0, 1, 2, 2, 0xAA, 0xAA,
        ]);
        assert_eq!(wire, expected);
    }

    // --- reach state ---------------------------------------------------------

    fn entry(class: ReachClass, scope: &[u8], version: u32) -> ReachStateEntry {
        ReachStateEntry {
            class,
            scope_id: scope.to_vec(),
            version,
        }
    }

    fn state() -> ReachState {
        ReachState {
            entries: vec![
                entry(ReachClass::Contact, b"", 3),
                entry(ReachClass::Fof, b"", 9),
                entry(ReachClass::Group, b"g1", 1),
                entry(ReachClass::Member, b"g1", 2),
                entry(ReachClass::Channel, b"c1", u32::MAX),
                entry(ReachClass::Link, &[7; MAX_REACH_SCOPE_ID_LEN], 0),
                entry(ReachClass::Reply, &[8; 16], 0),
            ],
            updated_at_ms: 77,
        }
    }

    fn raw_state(s: &ReachState) -> Vec<u8> {
        let mut out = REACH_STATE_ENVELOPE_DOMAIN_V1.to_vec();
        out.push(REACH_STATE_VERSION_V1);
        out.extend_from_slice(&s.updated_at_ms.to_be_bytes());
        out.extend_from_slice(&u16::try_from(s.entries.len()).unwrap().to_be_bytes());
        for e in &s.entries {
            out.push(e.class.to_byte());
            out.push(u8::try_from(e.scope_id.len()).unwrap());
            out.extend_from_slice(&e.scope_id);
            out.extend_from_slice(&e.version.to_be_bytes());
        }
        out
    }

    #[test]
    fn reach_state_round_trips() {
        let s = state();
        let wire = compose_reach_state(&s).unwrap();
        assert_eq!(wire, raw_state(&s));
        assert_eq!(decode_reach_state(&wire).unwrap(), s);
        assert_eq!(s.version_of(ReachClass::Member, b"g1"), Some(2));
        assert_eq!(s.version_of(ReachClass::Member, b"g2"), None);
        let empty = ReachState {
            entries: Vec::new(),
            updated_at_ms: 0,
        };
        assert_eq!(
            decode_reach_state(&compose_reach_state(&empty).unwrap()).unwrap(),
            empty
        );
    }

    #[test]
    fn reach_state_bounds_are_refused_by_compose_and_decode_alike() {
        let cases = [
            (
                vec![
                    entry(ReachClass::Group, b"g", 1),
                    entry(ReachClass::Group, b"g", 2),
                ],
                VisibilityError::DuplicateEntry,
            ),
            (
                vec![entry(ReachClass::Contact, b"x", 1)],
                VisibilityError::Reach(ReachError::ScopeIdNotEmpty),
            ),
            (
                vec![entry(ReachClass::Group, b"", 1)],
                VisibilityError::Reach(ReachError::ScopeIdEmpty),
            ),
            (
                vec![entry(
                    ReachClass::Group,
                    &[1; MAX_REACH_SCOPE_ID_LEN + 1],
                    1,
                )],
                VisibilityError::Reach(ReachError::ScopeIdTooLong {
                    len: MAX_REACH_SCOPE_ID_LEN + 1,
                }),
            ),
            (
                (0..=MAX_REACH_STATE_ENTRIES)
                    .map(|i| {
                        entry(
                            ReachClass::Link,
                            &u32::try_from(i).unwrap().to_be_bytes(),
                            0,
                        )
                    })
                    .collect(),
                VisibilityError::TooManyEntries {
                    count: MAX_REACH_STATE_ENTRIES + 1,
                },
            ),
        ];
        for (entries, why) in cases {
            let s = ReachState {
                entries,
                updated_at_ms: 1,
            };
            assert_eq!(compose_reach_state(&s), Err(why));
            assert_eq!(decode_reach_state(&raw_state(&s)), Err(why));
        }
        // The same scope under two classes is two entries.
        let ok = ReachState {
            entries: vec![
                entry(ReachClass::Group, b"g", 1),
                entry(ReachClass::Member, b"g", 1),
            ],
            updated_at_ms: 1,
        };
        compose_reach_state(&ok).unwrap();
    }

    #[test]
    fn reach_state_framing_is_fail_closed() {
        let wire = compose_reach_state(&state()).unwrap();
        let d = REACH_STATE_ENVELOPE_DOMAIN_V1.len();
        for cut in d..wire.len() {
            assert_eq!(
                try_unwrap_reach_state(&wire[..cut]),
                Err(VisibilityError::Truncated),
                "cut at {cut}"
            );
        }
        let mut long = wire.clone();
        long.push(0);
        assert_eq!(
            decode_reach_state(&long),
            Err(VisibilityError::TrailingBytes { count: 1 })
        );
        let mut v = wire.clone();
        v[d] = 0;
        assert_eq!(
            decode_reach_state(&v),
            Err(VisibilityError::UnknownVersion(0))
        );
        let mut c = wire.clone();
        c[d + 11] = 0x42;
        assert_eq!(
            decode_reach_state(&c),
            Err(VisibilityError::Reach(ReachError::UnknownClass(0x42)))
        );
        let mut huge = wire;
        huge[d + 9..d + 11].copy_from_slice(&u16::MAX.to_be_bytes());
        assert_eq!(
            decode_reach_state(&huge),
            Err(VisibilityError::TooManyEntries {
                count: usize::from(u16::MAX)
            })
        );
        assert_eq!(try_unwrap_reach_state(b"hello"), Ok(None));
        assert_eq!(
            decode_reach_state(b"hello"),
            Err(VisibilityError::WrongDomain)
        );
    }

    /// Every single-byte substitution of a valid wire: decode never panics,
    /// and whatever still decodes re-composes to the identical bytes.
    #[test]
    fn every_single_byte_mutation_decodes_canonically_or_is_refused() {
        let mut setting = VisibilitySetting::default_for_new_account();
        setting.chosen = vec![
            scope(ScopeKind::Group, 1, 3),
            scope(ScopeKind::Channel, 2, 2),
        ];
        let wire = compose_visibility(&setting).unwrap();
        for at in 0..wire.len() {
            for byte in 0..=u8::MAX {
                let mut m = wire.clone();
                m[at] = byte;
                if let Ok(s) = decode_visibility(&m) {
                    assert_eq!(compose_visibility(&s).unwrap(), m);
                }
            }
        }
        let wire = compose_reach_state(&state()).unwrap();
        for at in 0..wire.len() {
            for byte in 0..=u8::MAX {
                let mut m = wire.clone();
                m[at] = byte;
                if let Ok(s) = decode_reach_state(&m) {
                    assert_eq!(compose_reach_state(&s).unwrap(), m);
                }
            }
        }
    }

    #[test]
    fn orders_are_strict() {
        assert!(visibility_is_newer(2, 1));
        assert!(!visibility_is_newer(1, 1));
        assert!(reach_state_is_newer(2, 1));
        assert!(!reach_state_is_newer(1, 1));
        assert!(!reach_state_is_newer(0, 1));
    }

    #[test]
    fn error_codes_are_frozen() {
        assert_eq!(VisibilityError::SomeWithNoPart.code(), "someWithNoPart");
        assert_eq!(
            VisibilityError::Reach(ReachError::ScopeIdEmpty).code(),
            "reach.scopeIdEmpty"
        );
        assert_eq!(VisibilityError::DuplicateEntry.code(), "duplicateEntry");
    }

    #[test]
    fn domains_are_distinct_from_every_sibling() {
        let other_domains: &[&[u8]] = &[
            crate::call_welcome::CALL_WELCOME_ENVELOPE_DOMAIN_V1,
            crate::call_kp_forward::CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1,
            crate::media_state::MEDIA_STATE_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_GID_DERIVE_DOMAIN_V1,
            crate::committer_beacon::COMMITTER_BEACON_ENVELOPE_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
            crate::profile_request::PROFILE_REQUEST_ENVELOPE_DOMAIN_V1,
            crate::group_name::GROUP_NAME_ENVELOPE_DOMAIN_V1,
            crate::pin::PIN_ENVELOPE_DOMAIN_V1,
            crate::committer_delegation::COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1,
            crate::introduction::INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1,
            crate::introduction::INTRODUCTIONS_ENVELOPE_DOMAIN_V1,
            crate::invite::REVOKE_DOMAIN_V1,
            crate::invite::OWNER_LIST_DOMAIN_V1,
            crate::invite::JOINER_TAG_SALT_DOMAIN_V1,
            crate::call_invite::CALL_INVITE_REVOKE_DOMAIN_V1,
            crate::group_invite::GROUP_INVITE_REVOKE_DOMAIN_V1,
            b"lakko/group-join-attribution/v1",
        ];
        for mine in [
            VISIBILITY_ENVELOPE_DOMAIN_V1,
            REACH_STATE_ENVELOPE_DOMAIN_V1,
        ] {
            for d in other_domains {
                assert!(
                    !d.starts_with(mine) && !mine.starts_with(d),
                    "neither separator may prefix the other: {d:?}",
                );
            }
        }
        assert!(!VISIBILITY_ENVELOPE_DOMAIN_V1.starts_with(REACH_STATE_ENVELOPE_DOMAIN_V1));
        assert!(!REACH_STATE_ENVELOPE_DOMAIN_V1.starts_with(VISIBILITY_ENVELOPE_DOMAIN_V1));
    }
}
