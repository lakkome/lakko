<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-attachments/src/checklist.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if checklist.rs changes and this file does not. -->

# `checklist` — checklist body, author signature and fold (spec)

**Module:** `crates/lakko-attachments/src/checklist.rs`
**One line:** The canonical binary checklist body of ADR-0092 section 5, its author signature, and the fold of edits, added items, ticks and closes.

## Why this module exists

A checklist is edited, extended and ticked by several people over time; every client must show the same items, the same ticks and the same "done by". The fold is written once here so the clients cannot disagree.

## Public contract

- `ChecklistBody { title, others_add, others_mark, items: Vec<ChecklistItem { id: u32, text }> }` — `validate` / `encode` / `decode`, strict and canonical; item ids unique; 1..=30 items.
- `SignedChecklist { body, author_sig: Option<[u8; 64]> }` — `sign`, `unsigned` (channel post), `verify(group_id, author_key)`.
- `ChecklistItemRef::{Author(u32), Added(i64)}` — an author item by id, an added item by the DS `messageId` of its add row; the two never collide.
- `fold(body, author, checklist_message_id, edits, adds, ticks, closes) -> ChecklistState` with `ChecklistState { title, others_add, others_mark, items, closed_by }` and `done_count()`.

## Wire layout

`"lakko/checklist/v1\0" || 0x01 || flags(bit0 others_add, bit1 others_mark) || title_len(u8) || title || item_count(u8: 1..=30) || (item_id(u32 BE) || len(u8) || text)*`. Signature: Ed25519 over `"lakko/checklist/author-sig/v1\0" || group_id(32) || body`. Title 1..=255 bytes and item text 1..=100 bytes of display text (no control, no profile-table format character).

## Invariants upheld

- **C-68**: editors, adders, tickers and closers are account keys the caller resolved at receipt.
- Events are applied in DS `messageId` order (ties broken by a byte key so input order never matters): only the author's edits apply; an edit replaces title, flags, item texts and order, keeps added items, and drops ticks on items it removed; an add is allowed for the author, a privileged actor (a channel publisher, marked by the caller) or anyone while `others_add` holds, up to 30 items in all; a tick is allowed for the author, a privileged actor or anyone while `others_mark` holds, only on an existing item, last writer wins per item.
- Only ops after the checklist row and before the author's earliest close count; a close by anyone else is ignored; closing is final.

## Failure modes

`decode` returns a `ContentError` and never panics; `verify` returns `SignatureMissing` / `SignatureInvalid`.

## What this module does NOT do

It does not verify the signatures of edit payloads (the caller verifies each `SignedChecklist` before recording an edit) and does not know whether a conversation is a channel (the caller sets `privileged`).

## Attacker exposure

The body arrives base64 in a chat envelope any member writes; bounded by `MAX_CHECKLIST_BODY_LEN`. Fuzzed through `message_body_parse` (`fuzz_api::content_within_bounds`) and the module's proptests.

## References

ADR-0092 section 5; C-68; tests in `checklist.rs` and `tests/attachment_kinds.rs`.
