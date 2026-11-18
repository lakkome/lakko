// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The rows of a channel's subscriber list
//! (`docs/ux/group-and-channel-privacy.md` sections 15.1 and 7.3).
//!
//! The list is built from two sources a client reads separately:
//!
//! * the member cards it could open: one per subscriber key, each bound to
//!   the account it came from, carrying a name or not;
//! * in a private channel, for the owner and publishers, every registered
//!   subscriber key, each with the link it came by.
//!
//! What a person sees:
//!
//! * one row per person who shares a name. A card that carries no name is
//!   not such a row (base walk 2026-11-13: a nameless card read "Member",
//!   and the owner could not tell whom it was);
//! * "{n} others.": the subscribers who are not shown by name;
//! * in a private channel, for the owner and publishers, those others listed
//!   one row each, and `Remove` on every row the viewer may remove, named
//!   rows included. A key shown on a named row is never listed again among
//!   the others (the same walk listed both subscribers a second time as
//!   anonymous rows, so two `Remove` buttons could take the same person).
//!
//! Keys are compared without regard to ASCII case. The rule keeps the order
//! the client passes; sorting by name is the client's.

use std::collections::{HashMap, HashSet};

/// One member card the viewer could open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriberCard {
    /// The subscriber key the card is stored under (hex).
    pub subscriber_key: String,
    /// The account the card is bound to. Every card of one account is one
    /// person; an empty key stands alone.
    pub person_key: String,
    /// The card carries something to call the person by: a display name, or
    /// a username the viewer may show.
    pub named: bool,
    /// The card is the viewer's own account.
    pub is_self: bool,
}

/// One person shown by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriberPerson {
    /// Indices into the cards passed, every card of this person, in order.
    /// The first is the one to draw the row from.
    pub cards: Vec<u32>,
    /// The viewer may remove this person: `Remove` removes the subscriber key
    /// of every card in `cards`.
    pub removable: bool,
}

/// The subscriber list as a person sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriberList {
    /// The people shown by name, in the order of their first card.
    pub people: Vec<SubscriberPerson>,
    /// Indices into the listed keys passed: the subscribers listed one row
    /// each among the others, each with `Remove` (only when the viewer may
    /// remove). Never a key of a person shown by name, never twice.
    pub others: Vec<u32>,
    /// The number in "{n} others.": `0` says nothing.
    pub others_count: u64,
}

fn norm(key: &str) -> String {
    key.to_ascii_lowercase()
}

/// The rule in the module docs.
///
/// * `cards` — the member cards the viewer could open.
/// * `listed_keys` — every registered subscriber key the viewer read, for
///   the owner and publishers of a private channel; empty otherwise.
/// * `count` — the channel's subscriber count (one per registered key), or
///   `None` before it is known.
/// * `can_remove` — the viewer is the owner or a publisher of a private
///   channel.
#[must_use]
pub fn subscriber_list(
    cards: &[SubscriberCard],
    listed_keys: &[String],
    count: Option<u64>,
    can_remove: bool,
) -> SubscriberList {
    let mut people: Vec<SubscriberPerson> = Vec::new();
    let mut person_index: HashMap<String, usize> = HashMap::new();
    let mut covered: HashSet<String> = HashSet::new();
    let mut self_index: Option<usize> = None;

    for (i, card) in cards.iter().enumerate() {
        if !card.named && !card.is_self {
            continue;
        }
        let Ok(index) = u32::try_from(i) else { break };
        let key = norm(&card.subscriber_key);
        if !covered.insert(key) {
            // The same key twice: one card is enough.
            continue;
        }
        let slot = if card.is_self {
            self_index
        } else if card.person_key.is_empty() {
            None
        } else {
            person_index.get(&card.person_key).copied()
        };
        if let Some(at) = slot {
            people[at].cards.push(index);
            continue;
        }
        people.push(SubscriberPerson {
            cards: vec![index],
            removable: can_remove && !card.is_self,
        });
        let at = people.len() - 1;
        if card.is_self {
            self_index = Some(at);
        } else if !card.person_key.is_empty() {
            person_index.insert(card.person_key.clone(), at);
        }
    }

    let mut others: Vec<u32> = Vec::new();
    let mut listed: HashSet<String> = HashSet::new();
    for (i, key) in listed_keys.iter().enumerate() {
        let Ok(index) = u32::try_from(i) else { break };
        let key = norm(key);
        if covered.contains(&key) || !listed.insert(key) {
            continue;
        }
        others.push(index);
    }

    let from_count = count.map_or(0, |c| c.saturating_sub(covered.len() as u64));
    let others_count = if can_remove {
        // The full key list is read in pages: the count covers what is not
        // loaded yet, the rows cover a count that has not caught up.
        from_count.max(others.len() as u64)
    } else {
        from_count
    };

    SubscriberList {
        people,
        others,
        others_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn card(pk: &str, person: &str, named: bool) -> SubscriberCard {
        SubscriberCard {
            subscriber_key: pk.to_owned(),
            person_key: person.to_owned(),
            named,
            is_self: false,
        }
    }

    fn keys(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_base_walk_of_2026_11_13() {
        // Private channel, owner. The iPhone's card carries no name; the
        // Pixel uploaded none. Two keys, both by an invitation.
        let cards = vec![card("aa01", "iphone", false)];
        let list = subscriber_list(&cards, &keys(&["aa01", "bb02"]), Some(2), true);
        assert!(list.people.is_empty(), "a nameless card is not a named row");
        assert_eq!(list.others, vec![0, 1]);
        assert_eq!(list.others_count, 2);
    }

    #[test]
    fn a_named_person_is_not_listed_again_and_can_be_removed() {
        let cards = vec![card("bb02", "pixel", true), card("aa01", "iphone", false)];
        let list = subscriber_list(&cards, &keys(&["AA01", "BB02"]), Some(2), true);
        assert_eq!(list.people.len(), 1);
        assert_eq!(list.people[0].cards, vec![0]);
        assert!(list.people[0].removable);
        assert_eq!(list.others, vec![0], "only the iPhone among the others");
        assert_eq!(list.others_count, 1);
    }

    #[test]
    fn a_public_channel_removes_nobody_and_counts_the_others() {
        let cards = vec![card("bb02", "pixel", true), card("aa01", "iphone", false)];
        let list = subscriber_list(&cards, &[], Some(5), false);
        assert_eq!(list.people.len(), 1);
        assert!(!list.people[0].removable);
        assert!(list.others.is_empty());
        assert_eq!(list.others_count, 4);
    }

    #[test]
    fn every_device_of_one_person_is_one_row_and_removed_together() {
        let cards = vec![
            card("01", "pixel", true),
            card("02", "pixel", true),
            card("03", "", true),
        ];
        let list = subscriber_list(&cards, &keys(&["01", "02", "03", "04"]), Some(4), true);
        assert_eq!(list.people.len(), 2);
        assert_eq!(list.people[0].cards, vec![0, 1]);
        assert_eq!(list.people[1].cards, vec![2]);
        assert_eq!(list.others, vec![3]);
        assert_eq!(list.others_count, 1);
    }

    #[test]
    fn the_viewer_is_one_row_never_removable() {
        let mut own = card("01", "me", false);
        own.is_self = true;
        let mut own2 = card("02", "me", true);
        own2.is_self = true;
        let list = subscriber_list(&[own, own2], &keys(&["01", "02", "03"]), Some(3), true);
        assert_eq!(list.people.len(), 1);
        assert_eq!(list.people[0].cards, vec![0, 1]);
        assert!(!list.people[0].removable);
        assert_eq!(list.others, vec![2]);
        assert_eq!(list.others_count, 1);
    }

    #[test]
    fn a_partly_loaded_list_still_counts_from_the_service() {
        let list = subscriber_list(&[], &keys(&["01", "02"]), Some(120), true);
        assert_eq!(list.others, vec![0, 1]);
        assert_eq!(list.others_count, 120);
        let unknown = subscriber_list(&[], &keys(&["01", "01"]), None, true);
        assert_eq!(unknown.others, vec![0]);
        assert_eq!(unknown.others_count, 1);
        assert_eq!(subscriber_list(&[], &[], None, false).others_count, 0);
    }

    proptest! {
        #[test]
        fn no_key_is_both_named_and_listed(
            cards in proptest::collection::vec((0u8..8, 0u8..4, any::<bool>(), any::<bool>()), 0..12),
            listed in proptest::collection::vec(0u8..8, 0..12),
            count in proptest::option::of(0u64..40),
            can_remove in any::<bool>(),
        ) {
            let cards: Vec<SubscriberCard> = cards
                .into_iter()
                .map(|(pk, person, named, is_self)| SubscriberCard {
                    subscriber_key: format!("{pk:02x}"),
                    person_key: if person == 0 { String::new() } else { format!("p{person}") },
                    named,
                    is_self,
                })
                .collect();
            let listed: Vec<String> = listed.into_iter().map(|k| format!("{k:02X}")).collect();
            let list = subscriber_list(&cards, &listed, count, can_remove);
            let named: HashSet<String> = list
                .people
                .iter()
                .flat_map(|p| p.cards.iter().map(|&c| norm(&cards[c as usize].subscriber_key)))
                .collect();
            let mut seen = HashSet::new();
            for &o in &list.others {
                let key = norm(&listed[o as usize]);
                prop_assert!(!named.contains(&key));
                prop_assert!(seen.insert(key));
            }
            for p in &list.people {
                prop_assert!(!p.cards.is_empty());
                for &c in &p.cards {
                    let card = &cards[c as usize];
                    prop_assert!(card.named || card.is_self);
                }
                prop_assert!(!p.removable || can_remove);
            }
            if can_remove {
                prop_assert!(list.others_count >= list.others.len() as u64);
            }
        }
    }
}
