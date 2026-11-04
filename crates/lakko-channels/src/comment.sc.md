<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-channels/src/comment.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if comment.rs changes and this file does not. -->

# `comment` — sealed comments under a channel post (spec)

**Module:** `crates/lakko-channels/src/comment.rs`
**One line:** Seal and open a comment row's `sealed` field under a key from the post's epoch seed, signed inside the seal by a per-channel author key.

## Why this module exists

ADR-0087 section 13a: a subscriber's comment is a row `(post, cursor, writer_key, sealed)` at the channel service, at most 2,048 bytes; `writer_key` is the key of that one comment (`crate::write_key::CommentWriteKey`, from the account seed and a random comment nonce). The service must not read it or move it, readers must see it under a name and avatar its author chose, and a publisher must be able to ban an author without anyone learning an account.

## Public contract

- `Comment { name, avatar, text, reply_to }`, `Comment::validate` — name by the display-name rule; avatar an ADR-0084 still reference (square, 1..=640 px, 1..=256 KiB); text non-empty, trimmed, at most `COMMENT_TEXT_MAX_BYTES` (1,536), LINE FEED its only break, no control character, none of the refused bidi/invisible format characters (`lakko_contacts::channel_text`; ZWJ/ZWNJ allowed). `check_comment_text` is the text rule alone.
- `comment_author_seed` / `CommentAuthor::from_account_seed(account_seed, channel_id)` — `BLAKE3-derive-key("lakko/channel/comment-author/v1", account_seed ‖ channel_id)`; `public_key()` is what a ban list names.
- `CommentPlace { channel_id, epoch, post_cursor, writer_key }` — where a comment sits; both seal and open take it.
- `seal_comment(epoch_seed, place, author, comment)` / `open_comment(epoch_seed, place, sealed)` → `OpenedComment { author_key, comment }`.
- `comment_signed_bytes(place, body)` — what the author key signs.
- `CommentError::code()` / `CommentTextError::code()` — frozen reason codes (`text.empty`, `name.tooLong`, ...).

## Wire layout

```text
key       = HKDF-SHA-256(seed, salt = channel_id ‖ epoch_be ‖ post_cursor_be ‖ writer_key, info = "lakko/channel/comment-key/v1")
sealed    = 0x01 ‖ nonce(12, random) ‖ ChaCha20-Poly1305(plaintext) ‖ tag(16)
aad       = 0x01 ‖ channel_id ‖ epoch_be ‖ post_cursor_be ‖ writer_key
plaintext = author_key(32) ‖ sig(64) ‖ body
body      = name_len(1) ‖ name ‖ avatar_len(1)=0|84 ‖ avatar ‖ reply(1)=0|1 ‖ [reply_to(8)] ‖ text_len(2 BE) ‖ text
sig       = Ed25519_author("lakko/channel/comment-sig/v1" ‖ channel_id ‖ post_cursor_be ‖ writer_key ‖ BLAKE3(body))
```

Sealed length is `COMMENT_SEALED_MIN_LEN` (132) to `COMMENT_SEALED_CODEC_MAX_LEN` (2,013), checked at compile time to be within `COMMENT_SEALED_MAX_LEN` (2,048), the row cap the opener enforces.

## Invariants upheld

- A comment opens only at the place it was sealed for: another post, epoch, channel or writer key fails the authenticator (`a_comment_opens_only_where_it_was_written`).
- A reader who opened a comment cannot re-seal it under its own writer key or another post as the same author: the signature names both (`a_resealed_comment_under_another_writer_key_fails_the_author_signature`).
- The author key exists only inside the seal; the service never sees it. The plaintext carries no account key.
- Seal refuses exactly what open refuses (`seal_refuses_what_open_refuses`); the body encoding is canonical.

## Failure modes

`Wire` (length or version), `Open` (AEAD), `Malformed` (a length past the end, trailing bytes, avatar length not 0/84, reply flag not 0/1), `AuthorSignature` (undecodable or small-order author key, bad signature), `Name`, `Avatar`, `Text`; `KeyDerivation` / `Seal` cannot happen with valid inputs. No panic on any input.

## What this module does NOT do

No attachments (the ADR defers them), no edit, no timestamps (the service cursor orders comments). It does not hide that two comments in one channel are by one author from readers; that is what makes a ban possible. A publisher's delete is the existing publisher proof, not this module.

## Attacker exposure

Every reader opens bytes a stranger with a token (or the service) chose. Fuzz target `channel_comment_open` (`fuzz_api::channel_comment_open`) runs the open under a fixed seed and the plaintext and body parsers directly, asserting no acceptance by the AEAD and canonical re-encoding of anything a parser accepts.

## References

ADR-0087 section 13a; ADR-0084 (avatar reference); tests listed above and `a_maximal_comment_fits_the_row`, `the_body_parser_refuses_bad_flags_and_trailing_bytes`.
