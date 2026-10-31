// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A card sealed under its search terms (ADR-0086 section 2).
//!
//! The rows of a directory are numbered and the epoch key opens all of them,
//! and every contact of the owner holds that key. So a card sealed only by its
//! row could be read by any contact walking rows 0..N, with no search at all.
//! A card is therefore sealed by the person it describes, under keys only a
//! search term gives, and the owner of the directory stores it opaque.
//!
//! ```text
//! term_key := derive("lakko/circle/term-key/v1", oprf_output)
//! card_key := 32 random bytes, one per seal
//! sealed   := format(1) = 0x01 || salt(16) || slots(32 x MAX_CARD_TERMS)
//!             || nonce(12) || ciphertext
//! slot_i   := card_key XOR BLAKE3-keyed(term_key_i, "lakko/circle/card-slot/v1" || salt)
//! plain    := card_len(2) || card || zeros, CARD_PADDED_LEN bytes
//! aad      := "lakko/circle/card/v1\0" || format || salt || slots
//! ```
//!
//! Slots no term uses are random bytes, and the used ones sit in random order,
//! so every sealed card has one length and nothing shows how many terms it has
//! or how long the name is. The salt is fresh per seal, so a pad is never used
//! twice. A slot is 32 bytes rather than a 60-byte AEAD wrap because the slots
//! are most of a row and the rows are most of a directory's disk.
//!
//! **Why the slots are in the associated data.** A reader decrypts with the
//! key its own slot gives and looks at no other slot, so a slot left out of the
//! binding could be rewritten by anyone who stores the card, taking another
//! term off it, and the card would still open for this term. With every slot
//! bound, a change to any of them refuses the card for every term.
//!
//! **Reading.** A term key does not say which slot is its own, so a reader
//! tries each one. Up to [`MAX_CARD_TERMS`] AEAD trials on a card of this size
//! is a local cost, and a wrong candidate is an ordinary tag failure.
//!
//! **Key commitment.** ChaCha20-Poly1305 does not commit to its key, so whoever
//! seals could craft one ciphertext that opens to two cards under two terms.
//! That person writes the card anyway, so it gives them nothing they lack.
//!
//! [`open_card`] runs on bytes the Delivery Service returned and a contact
//! stored. Length and format are checked first, the padding after
//! authentication, and nothing panics (fuzz target `card_open`).

use std::collections::BTreeSet;

use lakko_crypto::aead::{self, AeadKey, Algorithm, Nonce, NONCE_LEN, TAG_LEN};
use lakko_crypto::hash::blake3_keyed;
use lakko_crypto::{random, SecretBytes};

use crate::codec::{pad, unpad, Reader, LEN_FIELD};
use crate::error::CircleError;
use crate::keys::{term_key, CARD_SLOT_DOMAIN, KEY_LEN, OPRF_OUTPUT_LEN, SALT_LEN};

const FORMAT_V1: u8 = 0x01;
const AAD_DOMAIN: &[u8] = b"lakko/circle/card/v1\0";
const SLOT_LEN: usize = KEY_LEN;

/// Slots in every sealed card: 64 name terms and the username, the most an
/// introduction record carries today
/// (`lakko_contacts::introduction::MAX_INTRODUCTION_TERMS`, pinned equal in the
/// FFI crates, which see both).
pub const MAX_CARD_TERMS: usize = 65;
/// Length of a card's padded plaintext. Chosen to hold the longest card
/// `lakko_contacts` can compose today (724 bytes), with room for the fields a
/// card still gains. Not measured against traffic: there is none.
pub const CARD_PADDED_LEN: usize = 768;
/// Longest card that can be sealed.
pub const CARD_MAX_LEN: usize = CARD_PADDED_LEN - LEN_FIELD;

const SLOTS_LEN: usize = SLOT_LEN * MAX_CARD_TERMS;

/// Length of every sealed card.
pub const SEALED_CARD_LEN: usize = 1 + SALT_LEN + SLOTS_LEN + NONCE_LEN + CARD_PADDED_LEN + TAG_LEN;

/// Whether `bytes` has the shape of a sealed card. No key is involved, so this
/// says nothing about what opens it; it stops a plain card reaching a row.
#[must_use]
pub fn is_sealed_card(bytes: &[u8]) -> bool {
    bytes.len() == SEALED_CARD_LEN && bytes.first() == Some(&FORMAT_V1)
}

/// What a term key XORs its slot with.
fn slot_pad(key: &SecretBytes<KEY_LEN>, salt: &[u8; SALT_LEN]) -> SecretBytes<KEY_LEN> {
    let mut input = Vec::with_capacity(CARD_SLOT_DOMAIN.len() + SALT_LEN);
    input.extend_from_slice(CARD_SLOT_DOMAIN);
    input.extend_from_slice(salt);
    SecretBytes::new(blake3_keyed(key.as_bytes(), &input))
}

fn xor(a: &[u8], mask: &SecretBytes<KEY_LEN>) -> SecretBytes<KEY_LEN> {
    let mut out = [0u8; KEY_LEN];
    for (byte, (x, y)) in out.iter_mut().zip(a.iter().zip(mask.as_bytes())) {
        *byte = x ^ y;
    }
    SecretBytes::new(out)
}

fn aad(salt: &[u8; SALT_LEN], slots: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(AAD_DOMAIN.len() + 1 + SALT_LEN + SLOTS_LEN);
    out.extend_from_slice(AAD_DOMAIN);
    out.push(FORMAT_V1);
    out.extend_from_slice(salt);
    out.extend_from_slice(slots);
    out
}

/// Seal `card` so that each of `term_outputs`, the OPRF outputs of the search
/// terms of the person it describes, opens it and nothing else does. A term
/// given twice takes one slot.
///
/// # Errors
///
/// [`CircleError::Empty`] with no term, [`CircleError::TooManyTerms`] past
/// [`MAX_CARD_TERMS`], [`CircleError::CardTooLong`] past [`CARD_MAX_LEN`],
/// [`CircleError::Seal`] on a cipher failure.
pub fn seal_card(
    card: &[u8],
    term_outputs: &[[u8; OPRF_OUTPUT_LEN]],
) -> Result<Vec<u8>, CircleError> {
    if term_outputs.is_empty() {
        return Err(CircleError::Empty);
    }
    if term_outputs.len() > MAX_CARD_TERMS {
        return Err(CircleError::TooManyTerms);
    }
    let padded = pad(card, CARD_PADDED_LEN)?;

    let mut salt = [0u8; SALT_LEN];
    random::fill(&mut salt);
    let card_key: AeadKey = random::secret_bytes::<KEY_LEN>();

    // Every slot starts as random bytes; the terms then take random places.
    let mut slots = vec![0u8; SLOTS_LEN];
    random::fill(&mut slots);
    let mut places: Vec<usize> = (0..MAX_CARD_TERMS).collect();
    places.sort_by_cached_key(|_| {
        let mut order = [0u8; 8];
        random::fill(&mut order);
        u64::from_be_bytes(order)
    });
    let distinct: BTreeSet<&[u8; OPRF_OUTPUT_LEN]> = term_outputs.iter().collect();
    for (output, place) in distinct.into_iter().zip(places) {
        let sealed_key = xor(card_key.as_bytes(), &slot_pad(&term_key(output), &salt));
        let slot = slots
            .chunks_exact_mut(SLOT_LEN)
            .nth(place)
            .ok_or(CircleError::Seal)?;
        slot.copy_from_slice(sealed_key.as_bytes());
    }

    let mut nonce: Nonce = [0u8; NONCE_LEN];
    random::fill(&mut nonce);
    let ciphertext = aead::seal(
        Algorithm::ChaCha20Poly1305,
        &card_key,
        &nonce,
        &aad(&salt, &slots),
        &padded,
    )
    .map_err(|_| CircleError::Seal)?;

    let mut out = Vec::with_capacity(SEALED_CARD_LEN);
    out.push(FORMAT_V1);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&slots);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    if out.len() == SEALED_CARD_LEN {
        Ok(out)
    } else {
        Err(CircleError::Seal)
    }
}

/// Open a sealed card with the OPRF output of a search term.
///
/// # Errors
///
/// [`CircleError::Length`], [`CircleError::UnknownFormat`],
/// [`CircleError::Authentication`] when this term is not on the card or the
/// bytes were changed, [`CircleError::Malformed`] on a padding that is not the
/// canonical one.
pub fn open_card(
    sealed: &[u8],
    oprf_output: &[u8; OPRF_OUTPUT_LEN],
) -> Result<Vec<u8>, CircleError> {
    open_with_term_key(sealed, &term_key(oprf_output))
}

/// [`open_card`] from the term key.
pub(crate) fn open_with_term_key(
    sealed: &[u8],
    key: &SecretBytes<KEY_LEN>,
) -> Result<Vec<u8>, CircleError> {
    if sealed.len() != SEALED_CARD_LEN {
        return Err(CircleError::Length);
    }
    let mut r = Reader::new(sealed);
    if r.u8()? != FORMAT_V1 {
        return Err(CircleError::UnknownFormat);
    }
    let salt: [u8; SALT_LEN] = r.array()?;
    let slots = r.take(SLOTS_LEN)?;
    let nonce: Nonce = r.array()?;
    let ciphertext = r.take(r.remaining())?;

    let binding = aad(&salt, slots);
    let term_pad = slot_pad(key, &salt);
    for slot in slots.chunks_exact(SLOT_LEN) {
        let candidate: AeadKey = xor(slot, &term_pad);
        if let Ok(padded) = aead::open(
            Algorithm::ChaCha20Poly1305,
            &candidate,
            &nonce,
            &binding,
            ciphertext,
        ) {
            return unpad(&padded, CARD_PADDED_LEN);
        }
    }
    Err(CircleError::Authentication)
}

/// The card inside a padded plaintext.
pub(crate) fn parse_padded(padded: &[u8]) -> Result<Vec<u8>, CircleError> {
    unpad(padded, CARD_PADDED_LEN)
}

#[cfg(test)]
mod tests {
    use lakko_crypto::hash::blake3_derive_key_from_material;
    use proptest::prelude::*;

    use super::*;
    use crate::keys::hex;

    fn output(byte: u8) -> [u8; OPRF_OUTPUT_LEN] {
        [byte; OPRF_OUTPUT_LEN]
    }

    fn outputs(count: u8) -> Vec<[u8; OPRF_OUTPUT_LEN]> {
        (0..count).map(output).collect()
    }

    fn fof_tag(oprf_output: &[u8; OPRF_OUTPUT_LEN]) -> [u8; 32] {
        blake3_derive_key_from_material("lakko/fof/tag/v1", oprf_output)
    }

    #[test]
    fn each_term_opens_the_card() {
        let terms = outputs(9);
        let sealed = seal_card(b"anna's card", &terms).unwrap();
        for term in &terms {
            assert_eq!(open_card(&sealed, term).unwrap(), b"anna's card");
        }
    }

    #[test]
    fn a_term_not_on_the_card_does_not_open_it() {
        let sealed = seal_card(b"anna's card", &outputs(9)).unwrap();
        assert_eq!(
            open_card(&sealed, &output(0xEE)),
            Err(CircleError::Authentication)
        );
    }

    #[test]
    fn a_tag_does_not_open_a_card() {
        let term = output(7);
        let tag = fof_tag(&term);
        assert_ne!(term_key(&term).as_bytes(), &tag);
        let sealed = seal_card(b"anna's card", &[term]).unwrap();
        assert_eq!(
            open_with_term_key(&sealed, &SecretBytes::new(tag)),
            Err(CircleError::Authentication)
        );
        // The control: this path does open with the term key itself.
        assert_eq!(
            open_with_term_key(&sealed, &term_key(&term)).unwrap(),
            b"anna's card"
        );
    }

    #[test]
    fn every_sealed_card_has_one_length() {
        let few = seal_card(b"", &outputs(1)).unwrap();
        let many = seal_card(&[7u8; CARD_MAX_LEN], &outputs(65)).unwrap();
        assert_eq!(few.len(), SEALED_CARD_LEN);
        assert_eq!(many.len(), SEALED_CARD_LEN);
    }

    #[test]
    fn every_term_of_a_full_card_has_a_slot() {
        let terms = outputs(65);
        let sealed = seal_card(b"full", &terms).unwrap();
        for term in &terms {
            assert_eq!(open_card(&sealed, term).unwrap(), b"full");
        }
    }

    #[test]
    fn two_seals_of_one_card_differ_everywhere_that_counts() {
        let terms = outputs(2);
        let a = seal_card(b"same", &terms).unwrap();
        let b = seal_card(b"same", &terms).unwrap();
        assert_ne!(a[1..=SALT_LEN], b[1..=SALT_LEN], "the salt");
        assert_ne!(a[1 + SALT_LEN..], b[1 + SALT_LEN..], "the rest");
    }

    #[test]
    fn the_used_slots_are_not_in_a_fixed_place() {
        // A card with one term puts its slot in any of the places: the reader
        // of that term sees which one, and with a fixed order it would learn
        // the term's rank. Twenty seals in one place is 65^-19.
        let term = output(3);
        let mut places = BTreeSet::new();
        for _ in 0..20 {
            let sealed = seal_card(b"x", &[term]).unwrap();
            let salt: [u8; SALT_LEN] = sealed[1..=SALT_LEN].try_into().unwrap();
            let slots = &sealed[1 + SALT_LEN..1 + SALT_LEN + SLOTS_LEN];
            let nonce: Nonce = sealed[1 + SALT_LEN + SLOTS_LEN..1 + SALT_LEN + SLOTS_LEN + 12]
                .try_into()
                .unwrap();
            let ciphertext = &sealed[1 + SALT_LEN + SLOTS_LEN + 12..];
            let term_pad = slot_pad(&term_key(&term), &salt);
            let binding = aad(&salt, slots);
            let place = slots.chunks_exact(SLOT_LEN).position(|slot| {
                aead::open(
                    Algorithm::ChaCha20Poly1305,
                    &xor(slot, &term_pad),
                    &nonce,
                    &binding,
                    ciphertext,
                )
                .is_ok()
            });
            places.insert(place.unwrap());
        }
        assert!(places.len() > 1);
    }

    #[test]
    fn a_term_given_twice_takes_one_slot_and_still_opens() {
        let term = output(5);
        let sealed = seal_card(b"x", &[term, term]).unwrap();
        assert_eq!(open_card(&sealed, &term).unwrap(), b"x");
        let slots = &sealed[1 + SALT_LEN..1 + SALT_LEN + SLOTS_LEN];
        let distinct: BTreeSet<&[u8]> = slots.chunks_exact(SLOT_LEN).collect();
        assert_eq!(distinct.len(), MAX_CARD_TERMS, "no two slots are equal");
    }

    #[test]
    fn any_single_byte_flip_is_refused() {
        // Under a term whose own slot is somewhere else too: a flip in any
        // other term's slot must refuse the card for this one.
        let terms = outputs(3);
        let sealed = seal_card(b"anna's card", &terms).unwrap();
        for at in 0..sealed.len() {
            let mut flipped = sealed.clone();
            flipped[at] ^= 0x01;
            assert!(open_card(&flipped, &terms[0]).is_err(), "byte {at}");
        }
    }

    #[test]
    fn the_refusals_of_seal() {
        assert_eq!(seal_card(b"x", &[]), Err(CircleError::Empty));
        assert_eq!(
            seal_card(b"x", &vec![output(1); MAX_CARD_TERMS + 1]),
            Err(CircleError::TooManyTerms)
        );
        assert_eq!(
            seal_card(&[0u8; CARD_MAX_LEN + 1], &outputs(1)),
            Err(CircleError::CardTooLong)
        );
    }

    #[test]
    fn the_refusals_of_open() {
        let sealed = seal_card(b"x", &outputs(1)).unwrap();
        assert_eq!(
            open_card(&sealed[1..], &output(0)),
            Err(CircleError::Length)
        );
        assert_eq!(open_card(&[], &output(0)), Err(CircleError::Length));
        let mut other_format = sealed;
        other_format[0] = 0x02;
        assert_eq!(
            open_card(&other_format, &output(0)),
            Err(CircleError::UnknownFormat)
        );
    }

    #[test]
    fn padding_is_canonical() {
        let mut padded = vec![0u8; CARD_PADDED_LEN];
        padded[1] = 1;
        padded[2] = b'x';
        assert_eq!(parse_padded(&padded), Ok(b"x".to_vec()));
        padded[100] = 1;
        assert_eq!(parse_padded(&padded), Err(CircleError::Malformed));
        let mut too_long = vec![0u8; CARD_PADDED_LEN];
        too_long[0] = 0xFF;
        assert_eq!(parse_padded(&too_long), Err(CircleError::Malformed));
    }

    #[test]
    fn the_derivations_are_pinned() {
        // A change to a context or an input layout must fail here, not on a
        // live directory whose cards all stop opening.
        let term = output(0x42);
        let key = term_key(&term);
        assert_eq!(
            hex(key.as_bytes()),
            "9cf72877abbf3fec91252bad1bc61e547612b0d0abe3024af2aaf1d7f18c67ad"
        );
        assert_eq!(
            hex(&fof_tag(&term)),
            "47b9f94eeabc18c6ca51fe6f2374c6e333dc34b20f2231d8ae58ad806134873d"
        );
        assert_eq!(
            hex(slot_pad(&key, &[0x24; SALT_LEN]).as_bytes()),
            "62edb076a5d4a99b6587f3ad5bfcb2b63ab82a3158c6eda11ef842751a457917"
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        /// Any card within the bound opens to itself under every one of its
        /// terms, at one length.
        #[test]
        fn a_card_round_trips(
            card in proptest::collection::vec(any::<u8>(), 0..=CARD_MAX_LEN),
            count in 1u8..=65,
        ) {
            let terms = outputs(count);
            let sealed = seal_card(&card, &terms).unwrap();
            prop_assert_eq!(sealed.len(), SEALED_CARD_LEN);
            let first = open_card(&sealed, &terms[0]).unwrap();
            prop_assert_eq!(&first, &card);
            let last = open_card(&sealed, &terms[usize::from(count) - 1]).unwrap();
            prop_assert_eq!(&last, &card);
        }

        /// `open_card` never panics, at any length.
        #[test]
        fn open_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
            let _ = open_card(&bytes, &output(1));
            let _ = parse_padded(&bytes);
        }

        /// Nor on bytes of exactly the sealed length, which reach the trials.
        #[test]
        fn open_never_accepts_bytes_it_did_not_seal(
            mut bytes in proptest::collection::vec(any::<u8>(), SEALED_CARD_LEN),
        ) {
            bytes[0] = FORMAT_V1;
            prop_assert_eq!(open_card(&bytes, &output(1)), Err(CircleError::Authentication));
        }
    }
}
