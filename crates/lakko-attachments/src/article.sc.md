<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-attachments/src/article.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if article.rs changes and this file does not. -->

# `article` — closed block format for long posts (spec)

**Module:** `crates/lakko-attachments/src/article.rs`
**One line:** Decode, encode and excerpt the ADR-0092 section 8 article: a bounded, canonical tree of paragraphs, headings, quotes, lists, images and dividers with bold, italic and https links.

## Why this module exists

An article is attacker-chosen formatted text that every client renders. HTML or Markdown would put a parser of hostile input in front of a web view and rely on a sanitiser listing what to forbid. A closed node set makes the forbidden things inexpressible; clients build native views from the decoded tree and never produce an HTML string.

## Public contract

- `Article { title, blocks }`, `Block::{Paragraph, Heading{level}, Quote, List{ordered, items}, Image{attachment_index, caption}, Divider}`, `Run::{Text{marks, text}, Link{url, marks, text}}`, `Marks{bold, italic}`, `ListItem{depth, runs}`, `HeadingLevel::{H1, H2}`.
- `validate` / `encode` / `decode`: `encode` refuses what `decode` refuses; a decoded article re-encodes to the identical bytes.
- `check_images(attachments)`: every image block points at an attachment whose MIME type the inline allowlist accepts as an image.
- `excerpt()`: paragraph, heading, quote and list-item text in order, joined by one space, cut to 200 characters; captions excluded.
- `article_url_valid(url)`: lower-case `https://` and the whole URL one link under the message-link rule (`crate::message_links::is_whole_link`, the one definition `lakko-ui-rules` re-exports): at most 2048 bytes, no white space, control or format character, a non-empty host naming no user, no trailing sentence punctuation.

## Wire layout

See the module doc: domain `"lakko/article/v1\0"`, version `0x01`, title (u8 length), block count (u16 BE), blocks by tag 1..=6, inlines as a u16 run count and runs tagged 1 (text) or 2 (link).

## Invariants upheld

- Bounds: encoded at most 131,072 bytes; run text at most 65,536 bytes in all; 1..=2048 blocks; at most 256 runs per block (a list counts all its items); at most 20 image blocks with distinct indices; title 1..=255 bytes.
- Canonical: no empty run; no adjacent text runs with equal marks; no adjacent link runs with equal URL and marks; no empty paragraph, heading, quote, list or list item (an image caption may be empty); a list's first item at depth 0, others at 0 or 1; heading level 1 or 2; no unknown tag, mark bit or ordered byte.
- No text in a run or the title holds a control or profile-table format character; a line break is a block boundary, never a character.

## Failure modes

Every refusal is a `ContentError`; `decode` checks the input length before parsing, bounds every count before allocating more than it describes, and never panics.

## What this module does NOT do

It does not render, does not fetch links and does not judge whether a link is safe; it does not read attachments beyond their MIME type.

## Attacker exposure

The article rides base64 in a chat envelope any member or publisher writes. Fuzz target `article_decode` (asserting `fuzz_api::article_decode_round_trip`), the envelope target `message_body_parse`, and proptests (round trip, canonical re-encode, hostile bytes, flipped bytes).

## References

ADR-0092 section 8; UX `docs/ux/attachments.md` section 11; tests in `article.rs`.
