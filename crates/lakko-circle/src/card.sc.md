<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC: regenerate from the source when card.rs changes -->

# `card` — a card sealed under its search terms (spec)

**Module:** `crates/lakko-circle/src/card.rs`   **One line:** a card that only a search term opens, so a contact holding the epoch key cannot read a directory by walking its rows.

## Why this module exists

The rows of a directory are numbered and the epoch key opens every row, and every contact of the owner holds that key. A row alone would hand all of an owner's contacts to any one of them, with no search and no token. A card is therefore sealed by the person it describes, under keys only a search term gives. The owner stores it and cannot read it.

## Public contract

- `seal_card(card, term_outputs)`: seals under the OPRF outputs of the person's terms. A term given twice takes one slot. Refuses no term, more than `MAX_CARD_TERMS`, and a card past `CARD_MAX_LEN`.
- `open_card(sealed, oprf_output)`: opens with the OPRF output of one term.
- `keys::term_key(oprf_output)`: the key a term opens a card with. Not the term tag, which comes from the same output under another context.
- `SEALED_CARD_LEN`: the one length of every sealed card, and what a row carries.

## Wire layout

```text
term_key := derive("lakko/circle/term-key/v1", oprf_output)
card_key := 32 random bytes, one per seal
sealed   := format(1) = 0x01 || salt(16) || slots(32 x MAX_CARD_TERMS) || nonce(12) || ciphertext
slot_i   := card_key XOR BLAKE3-keyed(term_key_i, "lakko/circle/card-slot/v1" || salt)
plain    := card_len(2) || card || zeros, CARD_PADDED_LEN bytes
aad      := "lakko/circle/card/v1\0" || format || salt || slots
```

Unused slots are random bytes and the used ones sit in random order.

## Invariants upheld

- The epoch key opens no card, and neither does a term tag; a row holds only sealed bytes (`a_contact_walking_every_row_reads_no_card_without_its_term`, `a_tag_does_not_open_a_card`).
- Every sealed card has one length, so nothing shows the number of terms or the length of the name (`every_sealed_card_has_one_length`).
- The salt is fresh per seal, so a pad is never used twice, and two seals of one card share nothing (`two_seals_of_one_card_differ_everywhere_that_counts`).
- Every slot is in the associated data, so changing any one refuses the card for every term, including terms whose slot it is not (`any_single_byte_flip_is_refused`).

## Failure modes

`Empty`, `TooManyTerms` and `CardTooLong` on seal; `Seal` on a cipher failure. On open: `Length` and `UnknownFormat` before anything is decrypted, `Authentication` when no slot gives a key that opens the ciphertext, `Malformed` on a padding that is not canonical. Nothing panics.

## What this module does NOT do

- It does not stop a member who holds tokens from asking the OPRF for a guessed term: walking prefixes from 3 letters collects the cards carrying them, at the rate the `discovery-lookup` quota allows.
- It does not stop the operator, who holds the OPRF key, once it also has a member's epoch key.
- It does not take back a card already opened.
- It does not commit to its key: whoever seals could craft a ciphertext that opens to two cards under two terms. That person writes the card anyway.
- `row` does not check that what it carries was sealed here: a plain card put in a row is readable by every contact. The clients are the ones that must pass a sealed card.

## Attacker exposure

`open_card` runs on bytes the Delivery Service returned and that a contact stored. A reader does up to `MAX_CARD_TERMS` AEAD trials on one fixed length, so a hostile card costs a bounded amount of local work. Fuzz target `card_open`, which also drives the padding parser behind the AEAD and a seal-open round trip.

## References

ADR-0086 section 2; `a_card_round_trips`, `open_never_panics`, `open_never_accepts_bytes_it_did_not_seal`, `the_derivations_are_pinned`.

`is_sealed_card` says whether bytes have a sealed card's length and format byte. It involves no key. `DirectoryWriter::put_row` refuses anything else with `CardNotSealed`, so a plain card cannot reach a row, where every contact could read it.
