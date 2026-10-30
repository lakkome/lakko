// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! How taps on "Who can find me" turn into a setting.
//!
//! `docs/ux/who-can-find-me.md` sections 3, 4.1, 5, 5.1 and 11. Three
//! hand-written copies of these would drift, and the drift would be a person
//! hidden from, or shown to, people they did not choose. So the rules are
//! here, once:
//!
//! * choosing **Some people** restores the parts the person last had on, or
//!   the default parts if they never had any;
//! * choosing **Nobody** or **Everyone** from Some people remembers the parts
//!   first;
//! * turning off the last part that is on selects **Nobody**, remembering
//!   what was on; turning a part on from Nobody or Everyone selects Some
//!   people with that part;
//! * turning one group off under **All groups and channels** switches to
//!   **Only the ones I choose** with every other group and channel on;
//! * turning one contact off under **All my contacts** switches to **Only the
//!   ones I choose** with every other contact on.
//!
//! Every function takes a setting and returns a new one that passes
//! [`VisibilitySetting::validate`]. None of them touches `updated_at_ms`:
//! the caller stamps the result, since the clock is platform code.
//!
//! The shape and its validity rule live in `lakko_contacts::visibility`;
//! this module decides only what a tap means.

use lakko_contacts::visibility::{
    ContactsForm, GroupsForm, ScopeId, VisibilityError, VisibilityMode, VisibilitySetting,
    CONTACT_ACCOUNT_ID_LEN, MAX_CHOSEN_CONTACTS, MAX_CHOSEN_SCOPES,
};

/// The parts Some people falls back to when there is nothing to restore:
/// the contacts of all contacts and all groups and channels (UX section 1).
const DEFAULT_PARTS: (bool, ContactsForm, Option<GroupsForm>) =
    (true, ContactsForm::All, Some(GroupsForm::All));

/// Choose one of the three positions.
///
/// * Choosing the current position changes nothing.
/// * Leaving Some people saves its parts into the memory first.
/// * Choosing Some people restores the memory, or the default parts when
///   the memory is empty. The contacts form is kept in the setting itself,
///   so it comes back with the part.
#[must_use]
pub fn choose_mode(s: &VisibilitySetting, mode: VisibilityMode) -> VisibilitySetting {
    let mut out = s.clone();
    if s.mode == mode {
        return out;
    }
    match mode {
        VisibilityMode::Nobody | VisibilityMode::Everyone => {
            if s.mode == VisibilityMode::Some {
                out.last_contacts_of_contacts = s.contacts_of_contacts;
                out.last_groups = s.groups;
            }
            out.mode = mode;
            out.contacts_of_contacts = false;
            out.groups = None;
        },
        VisibilityMode::Some => {
            let (cofc, contacts_form, groups) =
                if s.last_contacts_of_contacts || s.last_groups.is_some() {
                    (s.last_contacts_of_contacts, s.contacts_form, s.last_groups)
                } else {
                    DEFAULT_PARTS
                };
            out.mode = VisibilityMode::Some;
            out.contacts_of_contacts = cofc;
            out.contacts_form = contacts_form;
            out.groups = groups;
        },
    }
    out
}

/// Turn part 2.1, contacts of my contacts, on or off.
#[must_use]
pub fn set_contacts_of_contacts(s: &VisibilitySetting, on: bool) -> VisibilitySetting {
    set_parts(s, Some(on), None)
}

/// Turn part 2.2, members of my groups and channels, on or off.
///
/// Turning it on restores the form last used (All when there is none).
#[must_use]
pub fn set_groups(s: &VisibilitySetting, on: bool) -> VisibilitySetting {
    set_parts(s, None, Some(on))
}

/// The one rule both part switches share.
fn set_parts(
    s: &VisibilitySetting,
    contacts_of_contacts: Option<bool>,
    groups_on: Option<bool>,
) -> VisibilitySetting {
    let mut out = s.clone();
    let restored_form = s.groups.or(s.last_groups).unwrap_or(GroupsForm::All);
    match s.mode {
        VisibilityMode::Some => {
            let cofc = contacts_of_contacts.unwrap_or(s.contacts_of_contacts);
            let groups = match groups_on {
                None => s.groups,
                Some(false) => None,
                Some(true) => Some(restored_form),
            };
            if groups.is_none() && s.groups.is_some() {
                // Remember the form being switched off, so turning the part
                // back on (or coming back to Some people) restores it.
                out.last_groups = s.groups;
            }
            if !cofc && groups.is_none() {
                // The last part that was on went off: that means nobody.
                // Remember what was on until this tap.
                out.last_contacts_of_contacts = s.contacts_of_contacts;
                out.last_groups = s.groups;
                out.mode = VisibilityMode::Nobody;
                out.contacts_of_contacts = false;
                out.groups = None;
            } else {
                out.contacts_of_contacts = cofc;
                out.groups = groups;
            }
        },
        VisibilityMode::Nobody | VisibilityMode::Everyone => {
            // Turning a part off outside Some people changes nothing; turning
            // one on selects Some people with that part alone.
            if contacts_of_contacts == Some(true) {
                out.mode = VisibilityMode::Some;
                out.contacts_of_contacts = true;
                out.groups = None;
            } else if groups_on == Some(true) {
                out.mode = VisibilityMode::Some;
                out.contacts_of_contacts = false;
                out.groups = Some(restored_form);
            }
        },
    }
    out
}

/// Choose All or Only the ones I choose. Meaningful only while part 2.2 is
/// on; otherwise nothing changes.
#[must_use]
pub fn set_groups_form(s: &VisibilitySetting, form: GroupsForm) -> VisibilitySetting {
    let mut out = s.clone();
    if s.mode == VisibilityMode::Some && s.groups.is_some() {
        out.groups = Some(form);
    }
    out
}

/// Choose All my contacts or Only the ones I choose. Meaningful only while
/// part 2.1 is on; otherwise nothing changes.
#[must_use]
pub fn set_contacts_form(s: &VisibilitySetting, form: ContactsForm) -> VisibilitySetting {
    let mut out = s.clone();
    if s.mode == VisibilityMode::Some && s.contacts_of_contacts {
        out.contacts_form = form;
    }
    out
}

/// The `Members can find me` switch turned off in a group or channel's info
/// while the setting is **All groups and channels** (UX section 5): switch to
/// **Only the ones I choose** with every other scope in `all_scopes` chosen
/// and `excluded` not.
///
/// Under Only the ones I choose it unchooses `excluded`; in any other state
/// the switch is not shown, and nothing changes.
///
/// # Errors
///
/// * [`VisibilityError::ScopeIdLength`] — a scope id out of bounds.
/// * [`VisibilityError::TooManyChosen`] — more scopes than the list holds.
pub fn exclude_scope_under_all(
    s: &VisibilitySetting,
    all_scopes: &[ScopeId],
    excluded: &ScopeId,
) -> Result<VisibilitySetting, VisibilityError> {
    match (s.mode, s.groups) {
        (VisibilityMode::Some, Some(GroupsForm::All)) => {
            let mut chosen: Vec<ScopeId> = Vec::new();
            for scope in all_scopes {
                scope.validate()?;
                if scope != excluded && !chosen.contains(scope) {
                    chosen.push(scope.clone());
                }
            }
            if chosen.len() > MAX_CHOSEN_SCOPES {
                return Err(VisibilityError::TooManyChosen {
                    count: chosen.len(),
                });
            }
            let mut out = s.clone();
            out.groups = Some(GroupsForm::Chosen);
            out.chosen = chosen;
            Ok(out)
        },
        (VisibilityMode::Some, Some(GroupsForm::Chosen)) => set_scope_chosen(s, excluded, false),
        _ => Ok(s.clone()),
    }
}

/// Switch one group or channel on or off in the chosen list (UX section 4,
/// and section 5 under Only the ones I choose). The list is kept whatever
/// the form, so this never changes the position or the parts.
///
/// # Errors
///
/// * [`VisibilityError::ScopeIdLength`] — `scope`'s id is out of bounds.
/// * [`VisibilityError::TooManyChosen`] — the list is full.
pub fn set_scope_chosen(
    s: &VisibilitySetting,
    scope: &ScopeId,
    on: bool,
) -> Result<VisibilitySetting, VisibilityError> {
    scope.validate()?;
    let mut out = s.clone();
    let present = out.chosen.contains(scope);
    if on && !present {
        if out.chosen.len() >= MAX_CHOSEN_SCOPES {
            return Err(VisibilityError::TooManyChosen {
                count: out.chosen.len() + 1,
            });
        }
        out.chosen.push(scope.clone());
    } else if !on && present {
        out.chosen.retain(|c| c != scope);
    }
    Ok(out)
}

/// The `Their contacts can find me` switch turned off on a contact's screen
/// while the part is **All my contacts** (UX section 5.1): switch to **Only
/// the ones I choose** with every other contact in `all_contact_ids` chosen
/// and `excluded` not.
///
/// Under Only the ones I choose it unchooses `excluded`; in any other state
/// the switch is not shown, and nothing changes.
///
/// # Errors
///
/// [`VisibilityError::TooManyChosenContacts`] — more contacts than the list
/// holds.
pub fn exclude_contact_under_all(
    s: &VisibilitySetting,
    all_contact_ids: &[[u8; CONTACT_ACCOUNT_ID_LEN]],
    excluded: &[u8; CONTACT_ACCOUNT_ID_LEN],
) -> Result<VisibilitySetting, VisibilityError> {
    match (s.mode, s.contacts_of_contacts, s.contacts_form) {
        (VisibilityMode::Some, true, ContactsForm::All) => {
            let mut chosen: Vec<[u8; CONTACT_ACCOUNT_ID_LEN]> = Vec::new();
            let mut seen = std::collections::BTreeSet::new();
            for id in all_contact_ids {
                if id != excluded && seen.insert(id) {
                    chosen.push(*id);
                }
            }
            if chosen.len() > MAX_CHOSEN_CONTACTS {
                return Err(VisibilityError::TooManyChosenContacts {
                    count: chosen.len(),
                });
            }
            let mut out = s.clone();
            out.contacts_form = ContactsForm::Chosen;
            out.chosen_contacts = chosen;
            Ok(out)
        },
        (VisibilityMode::Some, true, ContactsForm::Chosen) => {
            set_contact_chosen(s, excluded, false)
        },
        _ => Ok(s.clone()),
    }
}

/// Switch one contact on or off in the chosen list (UX section 4.1, and
/// section 5.1 under Only the ones I choose). The list is kept whatever the
/// form, so this never changes the position or the parts.
///
/// # Errors
///
/// [`VisibilityError::TooManyChosenContacts`] — the list is full.
pub fn set_contact_chosen(
    s: &VisibilitySetting,
    account_id: &[u8; CONTACT_ACCOUNT_ID_LEN],
    on: bool,
) -> Result<VisibilitySetting, VisibilityError> {
    let mut out = s.clone();
    let present = out.chosen_contacts.contains(account_id);
    if on && !present {
        if out.chosen_contacts.len() >= MAX_CHOSEN_CONTACTS {
            return Err(VisibilityError::TooManyChosenContacts {
                count: out.chosen_contacts.len() + 1,
            });
        }
        out.chosen_contacts.push(*account_id);
    } else if !on && present {
        out.chosen_contacts.retain(|c| c != account_id);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::visibility::ScopeKind;

    const MODES: [VisibilityMode; 3] = [
        VisibilityMode::Nobody,
        VisibilityMode::Some,
        VisibilityMode::Everyone,
    ];
    const FORMS: [Option<GroupsForm>; 3] = [None, Some(GroupsForm::All), Some(GroupsForm::Chosen)];
    const CONTACTS_FORMS: [ContactsForm; 2] = [ContactsForm::All, ContactsForm::Chosen];

    fn scope(byte: u8) -> ScopeId {
        ScopeId {
            kind: ScopeKind::Group,
            id: vec![byte; 8],
        }
    }

    const fn contact(byte: u8) -> [u8; CONTACT_ACCOUNT_ID_LEN] {
        [byte; CONTACT_ACCOUNT_ID_LEN]
    }

    /// Every valid setting over the small fields, with small chosen lists.
    fn every_valid_setting() -> Vec<VisibilitySetting> {
        let mut out = Vec::new();
        for mode in MODES {
            for cofc in [false, true] {
                for groups in FORMS {
                    for last_cofc in [false, true] {
                        for last_groups in FORMS {
                            for contacts_form in CONTACTS_FORMS {
                                let s = VisibilitySetting {
                                    mode,
                                    contacts_of_contacts: cofc,
                                    contacts_form,
                                    chosen_contacts: vec![contact(1), contact(2)],
                                    groups,
                                    chosen: vec![scope(1), scope(2)],
                                    last_contacts_of_contacts: last_cofc,
                                    last_groups,
                                    updated_at_ms: 7,
                                };
                                if s.validate().is_ok() {
                                    out.push(s);
                                }
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(out.len(), 7 * 6 * 2);
        out
    }

    fn check(out: &VisibilitySetting, from: &VisibilitySetting) {
        out.validate()
            .unwrap_or_else(|e| panic!("{e} from {from:?} -> {out:?}"));
        assert_eq!(out.updated_at_ms, from.updated_at_ms, "the caller stamps");
    }

    #[test]
    fn every_transition_from_every_valid_setting_is_valid() {
        let all = [scope(1), scope(2), scope(3)];
        let contacts = [contact(1), contact(2), contact(3)];
        for s in every_valid_setting() {
            for mode in MODES {
                check(&choose_mode(&s, mode), &s);
            }
            for on in [false, true] {
                check(&set_contacts_of_contacts(&s, on), &s);
                check(&set_groups(&s, on), &s);
                check(&set_scope_chosen(&s, &scope(3), on).unwrap(), &s);
                check(&set_scope_chosen(&s, &scope(1), on).unwrap(), &s);
                check(&set_contact_chosen(&s, &contact(3), on).unwrap(), &s);
                check(&set_contact_chosen(&s, &contact(1), on).unwrap(), &s);
            }
            for form in [GroupsForm::All, GroupsForm::Chosen] {
                check(&set_groups_form(&s, form), &s);
            }
            for form in CONTACTS_FORMS {
                check(&set_contacts_form(&s, form), &s);
            }
            check(&exclude_scope_under_all(&s, &all, &scope(2)).unwrap(), &s);
            check(
                &exclude_contact_under_all(&s, &contacts, &contact(2)).unwrap(),
                &s,
            );
        }
    }

    #[test]
    fn choosing_the_current_position_changes_nothing() {
        for s in every_valid_setting() {
            assert_eq!(choose_mode(&s, s.mode), s);
        }
    }

    #[test]
    fn leaving_some_remembers_the_parts_and_coming_back_restores_them() {
        for s in every_valid_setting()
            .into_iter()
            .filter(|s| s.mode == VisibilityMode::Some)
        {
            for away in [VisibilityMode::Nobody, VisibilityMode::Everyone] {
                let left = choose_mode(&s, away);
                assert_eq!(left.mode, away);
                assert!(!left.contacts_of_contacts && left.groups.is_none());
                assert_eq!(left.last_contacts_of_contacts, s.contacts_of_contacts);
                assert_eq!(left.last_groups, s.groups);
                let back = choose_mode(&left, VisibilityMode::Some);
                assert_eq!(back.contacts_of_contacts, s.contacts_of_contacts);
                assert_eq!(back.groups, s.groups);
                assert_eq!(back.chosen, s.chosen, "the chosen list survives");
                assert_eq!(back.contacts_form, s.contacts_form);
                assert_eq!(back.chosen_contacts, s.chosen_contacts);
            }
        }
    }

    #[test]
    fn nobody_to_everyone_keeps_the_memory() {
        let mut s = VisibilitySetting::default_for_new_account();
        s = set_groups_form(&s, GroupsForm::Chosen);
        let nobody = choose_mode(&s, VisibilityMode::Nobody);
        let everyone = choose_mode(&nobody, VisibilityMode::Everyone);
        assert_eq!(everyone.last_groups, Some(GroupsForm::Chosen));
        assert!(everyone.last_contacts_of_contacts);
        let back = choose_mode(&everyone, VisibilityMode::Some);
        assert_eq!(back.groups, Some(GroupsForm::Chosen));
    }

    #[test]
    fn some_with_an_empty_memory_restores_the_default_parts() {
        let s = VisibilitySetting {
            mode: VisibilityMode::Nobody,
            contacts_of_contacts: false,
            contacts_form: ContactsForm::Chosen,
            chosen_contacts: vec![contact(1)],
            groups: None,
            chosen: Vec::new(),
            last_contacts_of_contacts: false,
            last_groups: None,
            updated_at_ms: 0,
        };
        let some = choose_mode(&s, VisibilityMode::Some);
        assert!(some.contacts_of_contacts);
        assert_eq!(some.groups, Some(GroupsForm::All));
        // The default parts are 2.1.1 and 2.2.1, whatever form was left over;
        // the list is the person's and stays.
        assert_eq!(some.contacts_form, ContactsForm::All);
        assert_eq!(some.chosen_contacts, vec![contact(1)]);
    }

    #[test]
    fn the_contacts_form_comes_back_through_nobody_and_everyone() {
        let d = set_contacts_form(
            &VisibilitySetting::default_for_new_account(),
            ContactsForm::Chosen,
        );
        let nobody = choose_mode(&d, VisibilityMode::Nobody);
        let everyone = choose_mode(&nobody, VisibilityMode::Everyone);
        let back = choose_mode(&everyone, VisibilityMode::Some);
        assert!(back.contacts_of_contacts);
        assert_eq!(back.contacts_form, ContactsForm::Chosen);
        // Everyone reaches everyone, whatever form it carries.
        assert!(everyone.allows_contact(&contact(9)));
        assert!(!back.allows_contact(&contact(9)));
    }

    #[test]
    fn switching_contacts_off_and_on_keeps_its_form() {
        let d = VisibilitySetting::default_for_new_account();
        let chosen = set_contacts_form(&d, ContactsForm::Chosen);
        let off = set_contacts_of_contacts(&chosen, false);
        assert!(!off.contacts_of_contacts);
        assert_eq!(off.contacts_form, ContactsForm::Chosen);
        assert_eq!(
            set_contacts_of_contacts(&off, true).contacts_form,
            ContactsForm::Chosen
        );
        // Turning it on from Nobody brings the form back too.
        let nobody = choose_mode(&chosen, VisibilityMode::Nobody);
        let on = set_contacts_of_contacts(&nobody, true);
        assert_eq!(on.mode, VisibilityMode::Some);
        assert_eq!(on.contacts_form, ContactsForm::Chosen);
    }

    #[test]
    fn the_contacts_form_changes_only_while_the_part_is_on() {
        let d = VisibilitySetting::default_for_new_account();
        assert_eq!(
            set_contacts_form(&d, ContactsForm::Chosen).contacts_form,
            ContactsForm::Chosen
        );
        let no_part = set_contacts_of_contacts(&d, false);
        assert_eq!(set_contacts_form(&no_part, ContactsForm::Chosen), no_part);
        for away in [VisibilityMode::Nobody, VisibilityMode::Everyone] {
            let s = choose_mode(&d, away);
            assert_eq!(set_contacts_form(&s, ContactsForm::Chosen), s);
        }
    }

    #[test]
    fn excluding_one_contact_under_all_chooses_every_other() {
        let d = VisibilitySetting::default_for_new_account();
        let all = [contact(1), contact(2), contact(3), contact(2), contact(1)];
        let out = exclude_contact_under_all(&d, &all, &contact(2)).unwrap();
        assert_eq!(out.mode, VisibilityMode::Some);
        assert_eq!(out.contacts_form, ContactsForm::Chosen);
        assert_eq!(out.chosen_contacts, vec![contact(1), contact(3)]);
        assert_eq!(
            out.groups,
            Some(GroupsForm::All),
            "the other part is untouched"
        );
        assert!(out.chosen.is_empty(), "the groups list is untouched");
        assert!(!out.allows_contact(&contact(2)));
        assert!(out.allows_contact(&contact(1)));
        // Under Chosen it unchooses the one contact.
        let again = exclude_contact_under_all(&out, &all, &contact(1)).unwrap();
        assert_eq!(again.chosen_contacts, vec![contact(3)]);
        // Where the switch is not shown, nothing changes.
        let nobody = choose_mode(&d, VisibilityMode::Nobody);
        let everyone = choose_mode(&d, VisibilityMode::Everyone);
        let no_part = set_contacts_of_contacts(&d, false);
        for s in [nobody, everyone, no_part] {
            assert_eq!(exclude_contact_under_all(&s, &all, &contact(1)).unwrap(), s);
        }
    }

    #[test]
    fn excluding_one_group_never_touches_the_contacts() {
        let d = set_contacts_form(
            &VisibilitySetting::default_for_new_account(),
            ContactsForm::Chosen,
        );
        let d = set_contact_chosen(&d, &contact(1), true).unwrap();
        let out = exclude_scope_under_all(&d, &[scope(1), scope(2)], &scope(2)).unwrap();
        assert_eq!(out.contacts_form, ContactsForm::Chosen);
        assert_eq!(out.chosen_contacts, vec![contact(1)]);
    }

    #[test]
    fn nobody_chosen_changes_nothing_by_itself() {
        let d = set_contacts_form(
            &VisibilitySetting::default_for_new_account(),
            ContactsForm::Chosen,
        );
        let out = set_contact_chosen(&d, &contact(1), false).unwrap();
        assert_eq!(out, d);
        assert_eq!(out.mode, VisibilityMode::Some);
        assert!(out.contacts_of_contacts && out.chosen_contacts.is_empty());
    }

    #[test]
    fn contact_bounds_are_errors_not_invalid_settings() {
        let id = |i: usize| {
            let mut id = [0; CONTACT_ACCOUNT_ID_LEN];
            id[..8].copy_from_slice(&(i as u64).to_be_bytes());
            id
        };
        let d = VisibilitySetting::default_for_new_account();
        let mut full = d.clone();
        full.chosen_contacts = (0..MAX_CHOSEN_CONTACTS).map(id).collect();
        full.validate().unwrap();
        assert_eq!(
            set_contact_chosen(&full, &contact(0xEE), true),
            Err(VisibilityError::TooManyChosenContacts {
                count: MAX_CHOSEN_CONTACTS + 1
            })
        );
        // Removing from a full list, or re-adding a present id, is fine.
        let present = full.chosen_contacts[0];
        assert_eq!(set_contact_chosen(&full, &present, true).unwrap(), full);
        assert_eq!(
            set_contact_chosen(&full, &present, false)
                .unwrap()
                .chosen_contacts
                .len(),
            MAX_CHOSEN_CONTACTS - 1
        );
        let too_many: Vec<_> = (0..=MAX_CHOSEN_CONTACTS + 1).map(id).collect();
        assert!(matches!(
            exclude_contact_under_all(&d, &too_many, &too_many[0]),
            Err(VisibilityError::TooManyChosenContacts { .. })
        ));
    }

    #[test]
    fn choosing_a_contact_is_idempotent_and_never_moves_the_position() {
        for s in every_valid_setting() {
            let on = set_contact_chosen(&s, &contact(5), true).unwrap();
            assert_eq!(set_contact_chosen(&on, &contact(5), true).unwrap(), on);
            assert_eq!(on.mode, s.mode);
            assert_eq!(on.contacts_of_contacts, s.contacts_of_contacts);
            assert_eq!(on.contacts_form, s.contacts_form);
            assert_eq!(on.groups, s.groups);
            let off = set_contact_chosen(&on, &contact(5), false).unwrap();
            assert_eq!(off, s);
        }
    }

    #[test]
    fn turning_off_the_last_part_selects_nobody_and_remembers_it() {
        let d = VisibilitySetting::default_for_new_account();
        let only_groups = set_contacts_of_contacts(&d, false);
        assert_eq!(only_groups.mode, VisibilityMode::Some);
        assert_eq!(only_groups.groups, Some(GroupsForm::All));
        let nobody = set_groups(&only_groups, false);
        assert_eq!(nobody.mode, VisibilityMode::Nobody);
        assert!(!nobody.last_contacts_of_contacts);
        assert_eq!(nobody.last_groups, Some(GroupsForm::All));
        // Coming back to Some people restores what was on at the last tap.
        let back = choose_mode(&nobody, VisibilityMode::Some);
        assert!(!back.contacts_of_contacts);
        assert_eq!(back.groups, Some(GroupsForm::All));

        let only_cofc = set_groups(&d, false);
        let nobody = set_contacts_of_contacts(&only_cofc, false);
        assert_eq!(nobody.mode, VisibilityMode::Nobody);
        assert!(nobody.last_contacts_of_contacts);
    }

    #[test]
    fn turning_a_part_on_from_nobody_or_everyone_selects_some_with_that_part() {
        for from in [VisibilityMode::Nobody, VisibilityMode::Everyone] {
            let s = choose_mode(&VisibilitySetting::default_for_new_account(), from);
            let cofc = set_contacts_of_contacts(&s, true);
            assert_eq!(cofc.mode, VisibilityMode::Some);
            assert!(cofc.contacts_of_contacts);
            assert_eq!(cofc.groups, None);
            let groups = set_groups(&s, true);
            assert_eq!(groups.mode, VisibilityMode::Some);
            assert!(!groups.contacts_of_contacts);
            assert_eq!(groups.groups, Some(GroupsForm::All));
            // Turning a part off there changes nothing.
            assert_eq!(set_contacts_of_contacts(&s, false), s);
            assert_eq!(set_groups(&s, false), s);
        }
    }

    #[test]
    fn switching_groups_off_and_on_restores_its_form() {
        let d = VisibilitySetting::default_for_new_account();
        let chosen = set_groups_form(&d, GroupsForm::Chosen);
        let off = set_groups(&chosen, false);
        assert_eq!(off.groups, None);
        assert_eq!(off.last_groups, Some(GroupsForm::Chosen));
        assert_eq!(set_groups(&off, true).groups, Some(GroupsForm::Chosen));
    }

    #[test]
    fn the_form_changes_only_while_groups_is_on() {
        let d = VisibilitySetting::default_for_new_account();
        assert_eq!(
            set_groups_form(&d, GroupsForm::Chosen).groups,
            Some(GroupsForm::Chosen)
        );
        let no_groups = set_groups(&d, false);
        assert_eq!(set_groups_form(&no_groups, GroupsForm::Chosen), no_groups);
        let nobody = choose_mode(&d, VisibilityMode::Nobody);
        assert_eq!(set_groups_form(&nobody, GroupsForm::Chosen), nobody);
    }

    #[test]
    fn excluding_one_group_under_all_chooses_every_other() {
        let d = VisibilitySetting::default_for_new_account();
        let all = [scope(1), scope(2), scope(3), scope(2)];
        let out = exclude_scope_under_all(&d, &all, &scope(2)).unwrap();
        assert_eq!(out.mode, VisibilityMode::Some);
        assert_eq!(out.groups, Some(GroupsForm::Chosen));
        assert_eq!(out.chosen, vec![scope(1), scope(3)]);
        assert!(out.contacts_of_contacts, "the other part is untouched");
        // Under Chosen it unchooses the one scope.
        let again = exclude_scope_under_all(&out, &all, &scope(1)).unwrap();
        assert_eq!(again.chosen, vec![scope(3)]);
        // Where the switch is not shown, nothing changes.
        let nobody = choose_mode(&d, VisibilityMode::Nobody);
        assert_eq!(
            exclude_scope_under_all(&nobody, &all, &scope(1)).unwrap(),
            nobody
        );
    }

    #[test]
    fn scope_bounds_are_errors_not_invalid_settings() {
        let d = VisibilitySetting::default_for_new_account();
        let empty = ScopeId {
            kind: ScopeKind::Channel,
            id: Vec::new(),
        };
        assert_eq!(
            set_scope_chosen(&d, &empty, true),
            Err(VisibilityError::ScopeIdLength { len: 0 })
        );
        assert_eq!(
            exclude_scope_under_all(&d, &[empty.clone(), scope(1)], &scope(1)),
            Err(VisibilityError::ScopeIdLength { len: 0 })
        );
        let mut full = d.clone();
        full.chosen = (0..MAX_CHOSEN_SCOPES)
            .map(|i| ScopeId {
                kind: ScopeKind::Group,
                id: u32::try_from(i).unwrap().to_be_bytes().to_vec(),
            })
            .collect();
        full.validate().unwrap();
        assert_eq!(
            set_scope_chosen(&full, &scope(9), true),
            Err(VisibilityError::TooManyChosen {
                count: MAX_CHOSEN_SCOPES + 1
            })
        );
        // Removing from a full list, or re-adding a present scope, is fine.
        let present = full.chosen[0].clone();
        assert_eq!(set_scope_chosen(&full, &present, true).unwrap(), full);
        assert_eq!(
            set_scope_chosen(&full, &present, false)
                .unwrap()
                .chosen
                .len(),
            MAX_CHOSEN_SCOPES - 1
        );
        let too_many: Vec<ScopeId> = (0..=MAX_CHOSEN_SCOPES + 1)
            .map(|i| ScopeId {
                kind: ScopeKind::Group,
                id: u32::try_from(i).unwrap().to_be_bytes().to_vec(),
            })
            .collect();
        assert!(matches!(
            exclude_scope_under_all(&d, &too_many, &too_many[0]),
            Err(VisibilityError::TooManyChosen { .. })
        ));
    }

    #[test]
    fn choosing_a_scope_is_idempotent_and_never_moves_the_position() {
        for s in every_valid_setting() {
            let on = set_scope_chosen(&s, &scope(5), true).unwrap();
            assert_eq!(set_scope_chosen(&on, &scope(5), true).unwrap(), on);
            assert_eq!(on.mode, s.mode);
            assert_eq!(on.groups, s.groups);
            let off = set_scope_chosen(&on, &scope(5), false).unwrap();
            assert_eq!(off, s);
        }
    }
}
