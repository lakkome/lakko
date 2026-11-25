// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The list of pins each action writes (`docs/ux/navigation.md` section
//! 2.2.1, `docs/ux/chat-folders.md` section 2.3).
//!
//! Only pinned conversations are put in order by hand. A pin, an unpin, a
//! drag and Move up / Move down each write the WHOLE list of pins - the
//! account's `ConversationOrder` register for All chats, the folder's own pin
//! list inside a folder. Writing the whole list rather than a delta is what
//! folds pins made before the order existed into it at the first change, and
//! what lets one last-writer-wins register carry the order between devices.
//!
//! Every function takes two lists of row ids:
//!
//! - `stored`: the list as the synced register holds it, mapped to row ids.
//!   It may name conversations this device does not hold yet (a folder pin
//!   that arrived before its chat). Those entries are KEPT, at their place:
//!   a device must not unpin, in a folder, a conversation it has not seen.
//! - `shown`: the pins in the order the list draws them (the order rule of
//!   [`crate::conversation`] or [`crate::folder`] already applied). In All
//!   chats it may hold pins the register does not name; they are drawn above
//!   the named ones, and the first write puts them into the list there.
//!
//! Every function returns the list to write, with no repeated id.

/// Keep the first occurrence of each id.
fn dedup(ids: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in ids {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// The list as the screen implies it before any action: the shown pins the
/// register does not name (they are drawn first), then the register.
fn base(stored: &[String], shown: &[String]) -> Vec<String> {
    dedup(
        shown
            .iter()
            .filter(|s| !stored.contains(s))
            .chain(stored.iter())
            .cloned(),
    )
}

/// Pin `id`: it goes to the top of the pins.
#[must_use]
pub fn pin_order_after_pin(stored: &[String], shown: &[String], id: &str) -> Vec<String> {
    dedup(
        core::iter::once(id.to_string()).chain(base(stored, shown).into_iter().filter(|s| s != id)),
    )
}

/// Unpin `id`: it leaves the pins, the others keep their order.
#[must_use]
pub fn pin_order_after_unpin(stored: &[String], shown: &[String], id: &str) -> Vec<String> {
    base(stored, shown)
        .into_iter()
        .filter(|s| s != id)
        .collect()
}

/// Move the pin `id` so it lands at `to_index` among the SHOWN pins
/// (0 = first).
///
/// `to_index` is its index among the shown pins after the move, so a drop
/// below the last pin passes anything at or past the end and lands last, and
/// a drop above the first passes 0. Move up is `index - 1`, Move down
/// `index + 1`. Entries of `stored` that are not shown keep their places:
/// the shown pins are laid back into the slots they held, in their new order.
/// An `id` that is not shown is not a pin here, and a drag never pins: the
/// list comes back as it was.
#[must_use]
pub fn pin_order_after_move(
    stored: &[String],
    shown: &[String],
    id: &str,
    to_index: u32,
) -> Vec<String> {
    let mut list = base(stored, shown);
    let mut visible = dedup(shown.iter().cloned());
    let Some(from) = visible.iter().position(|s| s == id) else {
        return list;
    };
    let moved = visible.remove(from);
    let at = usize::try_from(to_index)
        .unwrap_or(usize::MAX)
        .min(visible.len());
    visible.insert(at, moved);
    let mut next = visible.into_iter();
    for slot in &mut list {
        if shown.contains(slot) {
            if let Some(v) = next.next() {
                *slot = v;
            }
        }
    }
    list
}

/// Whether the pin `id` offers Move up (`up`) or Move down: it is shown as a
/// pin, and it is not already first (up) or last (down). One pin offers
/// neither.
#[must_use]
pub fn pin_can_move(shown: &[String], id: &str, up: bool) -> bool {
    let pins = dedup(shown.iter().cloned());
    let Some(at) = pins.iter().position(|s| s == id) else {
        return false;
    };
    if up {
        at > 0
    } else {
        at + 1 < pins.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn a_new_pin_goes_on_top() {
        let pins = v(&["a", "b"]);
        assert_eq!(pin_order_after_pin(&pins, &pins, "c"), v(&["c", "a", "b"]));
    }

    #[test]
    fn pinning_a_pin_again_moves_it_on_top_once() {
        let pins = v(&["a", "b"]);
        assert_eq!(pin_order_after_pin(&pins, &pins, "b"), v(&["b", "a"]));
    }

    #[test]
    fn a_pin_the_register_does_not_name_is_written_where_it_was_drawn() {
        // "x" was pinned on another device; it is drawn above the named pins.
        let stored = v(&["a", "b"]);
        let shown = v(&["x", "a", "b"]);
        assert_eq!(
            pin_order_after_pin(&stored, &shown, "c"),
            v(&["c", "x", "a", "b"])
        );
        assert_eq!(pin_order_after_unpin(&stored, &shown, "a"), v(&["x", "b"]));
        assert_eq!(
            pin_order_after_move(&stored, &shown, "x", 2),
            v(&["a", "b", "x"])
        );
    }

    #[test]
    fn a_pin_this_device_does_not_hold_keeps_its_place() {
        // "u" is a folder pin whose chat has not arrived here.
        let stored = v(&["a", "u", "b", "c"]);
        let shown = v(&["a", "b", "c"]);
        assert_eq!(
            pin_order_after_move(&stored, &shown, "c", 0),
            v(&["c", "u", "a", "b"])
        );
        assert_eq!(
            pin_order_after_unpin(&stored, &shown, "b"),
            v(&["a", "u", "c"])
        );
        assert_eq!(
            pin_order_after_pin(&stored, &shown, "n"),
            v(&["n", "a", "u", "b", "c"])
        );
    }

    #[test]
    fn moves_land_where_they_are_dropped() {
        let pins = v(&["a", "b", "c", "d"]);
        assert_eq!(
            pin_order_after_move(&pins, &pins, "a", 2),
            v(&["b", "c", "a", "d"])
        );
        assert_eq!(
            pin_order_after_move(&pins, &pins, "d", 0),
            v(&["d", "a", "b", "c"])
        );
        assert_eq!(
            pin_order_after_move(&pins, &pins, "b", 99),
            v(&["a", "c", "d", "b"])
        );
        assert_eq!(pin_order_after_move(&pins, &pins, "zz", 0), pins);
    }

    #[test]
    fn move_up_and_down_are_offered_only_where_they_move() {
        let pins = v(&["a", "b", "c"]);
        assert!(!pin_can_move(&pins, "a", true));
        assert!(pin_can_move(&pins, "a", false));
        assert!(pin_can_move(&pins, "c", true));
        assert!(!pin_can_move(&pins, "c", false));
        assert!(!pin_can_move(&v(&["a"]), "a", true));
        assert!(!pin_can_move(&v(&["a"]), "a", false));
        assert!(!pin_can_move(&pins, "zz", true));
    }

    fn lists() -> impl proptest::strategy::Strategy<Value = (Vec<String>, Vec<String>)> {
        use proptest::prelude::*;
        // Ids p0..p11; each is stored or not, shown or not, in a random order.
        (
            proptest::collection::vec((any::<bool>(), any::<bool>()), 1..12),
            Just(()).prop_perturb(|(), mut rng| rng.next_u64()),
        )
            .prop_map(|(flags, seed)| {
                let mut stored = Vec::new();
                let mut shown = Vec::new();
                for (i, (st, sh)) in flags.iter().enumerate() {
                    let id = format!("p{i}");
                    if *st {
                        stored.push(id.clone());
                    }
                    if *sh {
                        shown.push(id);
                    }
                }
                // Rotate the stored list by the seed so it is not sorted.
                if !stored.is_empty() {
                    let k = usize::try_from(seed % stored.len() as u64).unwrap_or(0);
                    stored.rotate_left(k);
                }
                // Shown in the order the list draws: unnamed first, then the
                // register's order.
                let mut drawn: Vec<String> = shown
                    .iter()
                    .filter(|s| !stored.contains(s))
                    .cloned()
                    .collect();
                drawn.extend(stored.iter().filter(|s| shown.contains(s)).cloned());
                (stored, drawn)
            })
    }

    proptest::proptest! {
        #[test]
        fn a_move_puts_the_pin_at_its_target_and_keeps_everything_else(
            (stored, shown) in lists(),
            pick in 0usize..12,
            to in 0u32..20,
        ) {
            proptest::prop_assume!(!shown.is_empty());
            let id = shown[pick % shown.len()].clone();
            let out = pin_order_after_move(&stored, &shown, &id, to);
            // Nothing lost, nothing added.
            let mut a = out.clone();
            a.sort();
            let mut b = base(&stored, &shown);
            b.sort();
            proptest::prop_assert_eq!(a, b);
            // The pin is at its target among the shown ones.
            let visible: Vec<&String> = out.iter().filter(|s| shown.contains(s)).collect();
            let want = usize::try_from(to).unwrap_or(usize::MAX).min(shown.len() - 1);
            proptest::prop_assert_eq!(visible.iter().position(|s| **s == id), Some(want));
            // The other shown pins keep their relative order.
            let others = |l: &[&String]| -> Vec<String> { l.iter().filter(|s| ***s != id).map(|s| (*s).clone()).collect() };
            let before: Vec<&String> = shown.iter().collect();
            proptest::prop_assert_eq!(others(&visible), others(&before));
            // Entries not shown keep their slots.
            let base_list = base(&stored, &shown);
            for (i, s) in base_list.iter().enumerate() {
                if !shown.contains(s) {
                    proptest::prop_assert_eq!(&out[i], s);
                }
            }
        }

        #[test]
        fn move_up_then_down_is_the_identity_when_offered(
            (stored, shown) in lists(),
            pick in 0usize..12,
        ) {
            proptest::prop_assume!(!shown.is_empty());
            let at = pick % shown.len();
            let id = shown[at].clone();
            let at = u32::try_from(at).unwrap_or(0);
            let start = base(&stored, &shown);
            if pin_can_move(&shown, &id, true) {
                let up = pin_order_after_move(&stored, &shown, &id, at - 1);
                let shown_up: Vec<String> = up.iter().filter(|s| shown.contains(s)).cloned().collect();
                proptest::prop_assert_eq!(pin_order_after_move(&up, &shown_up, &id, at), start.clone());
            }
            if pin_can_move(&shown, &id, false) {
                let down = pin_order_after_move(&stored, &shown, &id, at + 1);
                let shown_down: Vec<String> = down.iter().filter(|s| shown.contains(s)).cloned().collect();
                proptest::prop_assert_eq!(pin_order_after_move(&down, &shown_down, &id, at), start);
            }
        }

        #[test]
        fn pin_then_unpin_leaves_the_others_in_order(
            (stored, shown) in lists(),
        ) {
            let fresh = "new".to_string();
            let pinned = pin_order_after_pin(&stored, &shown, &fresh);
            proptest::prop_assert_eq!(pinned.first(), Some(&fresh));
            let mut shown_after = vec![fresh.clone()];
            shown_after.extend(shown.iter().cloned());
            let unpinned = pin_order_after_unpin(&pinned, &shown_after, &fresh);
            proptest::prop_assert_eq!(unpinned, base(&stored, &shown));
        }
    }
}
