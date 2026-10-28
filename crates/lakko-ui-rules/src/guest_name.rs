// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The name prefilled for a guest who joins a call from a link.
//!
//! A visitor without an account gets "<adjective> <animal>" (`Calm Fox`)
//! instead of the same word for everybody, so the people in a call can tell
//! two guests apart. The word lists are the ones the earlier Lakko call
//! client used. The name becomes the guest account's profile name, so it
//! passes the display-name rule (`lakko_contacts::profile`); a test pins that.
//!
//! The randomness is the caller's: this crate holds no generator, and the
//! name is not a secret, so any random 64-bit value will do.

/// Adjectives, as the earlier client had them, without its repeats.
pub const GUEST_ADJECTIVES: &[&str] = &[
    "Calm",
    "Kind",
    "Friendly",
    "Gentle",
    "Polite",
    "Honest",
    "Helpful",
    "Patient",
    "Curious",
    "Creative",
    "Thoughtful",
    "Respectful",
    "Reliable",
    "Supportive",
    "Cooperative",
    "Receptive",
    "Positive",
    "Cheerful",
    "Attentive",
    "Understanding",
    "Peaceful",
    "Balanced",
    "Flexible",
    "Responsible",
    "Trustworthy",
    "Motivated",
    "Engaged",
    "Confident",
    "Mindful",
    "Empathetic",
    "Caring",
    "Fair",
    "Constructive",
    "Sincere",
    "Considerate",
    "Tolerant",
    "Approachable",
    "Composed",
    "Diligent",
    "Optimistic",
    "Encouraging",
    "Respectable",
    "Ethical",
    "Dependable",
    "Warmhearted",
    "Insightful",
    "Reflective",
    "Harmonious",
];

/// Animals, as the earlier client had them.
pub const GUEST_ANIMALS: &[&str] = &[
    "Dog",
    "Cat",
    "Lion",
    "Tiger",
    "Elephant",
    "Giraffe",
    "Zebra",
    "Horse",
    "Cow",
    "Sheep",
    "Goat",
    "Pig",
    "Rabbit",
    "Deer",
    "Bear",
    "Wolf",
    "Fox",
    "Monkey",
    "Gorilla",
    "Chimpanzee",
    "Kangaroo",
    "Koala",
    "Panda",
    "Camel",
    "Dolphin",
    "Whale",
    "Shark",
    "Octopus",
    "Penguin",
    "Eagle",
    "Hawk",
    "Owl",
    "Parrot",
    "Crow",
    "Snake",
    "Lizard",
    "Crocodile",
    "Frog",
    "Toad",
    "Turtle",
    "Butterfly",
    "Bee",
    "Ant",
    "Spider",
    "Crab",
    "Lobster",
    "Seal",
    "Otter",
    "Moose",
    "Squirrel",
];

/// The guest name for one random value: an adjective and an animal.
///
/// The low bits pick the adjective and the rest the animal, so every pair is
/// reachable from a uniformly random `random`.
#[must_use]
pub fn guest_name(random: u64) -> String {
    let adjectives = GUEST_ADJECTIVES.len() as u64;
    let animals = GUEST_ANIMALS.len() as u64;
    // The indices are below the list lengths, so the casts cannot truncate.
    #[allow(clippy::cast_possible_truncation)]
    let adjective = GUEST_ADJECTIVES[(random % adjectives) as usize];
    #[allow(clippy::cast_possible_truncation)]
    let animal = GUEST_ANIMALS[((random / adjectives) % animals) as usize];
    format!("{adjective} {animal}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::profile::check_display_name;
    use std::collections::HashSet;

    #[test]
    fn every_pair_passes_the_display_name_rule() {
        for adjective in GUEST_ADJECTIVES {
            for animal in GUEST_ANIMALS {
                let name = format!("{adjective} {animal}");
                assert!(check_display_name(&name).is_ok(), "{name}");
            }
        }
    }

    #[test]
    fn the_lists_hold_no_repeats() {
        assert_eq!(
            GUEST_ADJECTIVES.iter().collect::<HashSet<_>>().len(),
            GUEST_ADJECTIVES.len()
        );
        assert_eq!(
            GUEST_ANIMALS.iter().collect::<HashSet<_>>().len(),
            GUEST_ANIMALS.len()
        );
    }

    #[test]
    fn every_pair_is_reachable() {
        let a = GUEST_ADJECTIVES.len() as u64;
        let n = GUEST_ANIMALS.len() as u64;
        let names: HashSet<String> = (0..a * n).map(guest_name).collect();
        assert_eq!(names.len(), GUEST_ADJECTIVES.len() * GUEST_ANIMALS.len());
    }

    #[test]
    fn any_value_gives_a_name() {
        assert_eq!(guest_name(0), "Calm Dog");
        assert!(!guest_name(u64::MAX).is_empty());
    }
}
