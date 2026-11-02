<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-channels/src/ban_list.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if ban_list.rs changes and this file does not. -->

# `ban_list` — a public channel's ban list (spec)

**Module:** `crates/lakko-channels/src/ban_list.rs`
**One line:** The record a publisher posts to tell every reader whose comments to hide.

## Why this module exists

ADR-0087 section 13a: the channel service cannot refuse a comment (it names only a per-post key), and a public channel's read key cannot be withdrawn. A ban is therefore a list of comment author keys every reader applies. It rides as the plaintext of an ordinary post, like the metadata record, so the post's seal and publisher signature make it "sealed and publisher-signed".

## Public contract

- `BanList { updated_at_ms, authors }` — authors oldest first, no repeat, at most `BAN_LIST_MAX_KEYS` (1,024, chosen). `validate`, `is_banned`, `empty`.
- `with_banned(author, now_ms)` — appends a new key (an existing one keeps its place), drops the oldest past the cap; `without(author, now_ms)` — removes it. Both stamp `max(now_ms, updated_at_ms + 1)`, so a new list is always newer.
- `ban_list_is_newer(incoming, stored)` — strictly greater.
- `compose_ban_list` refuses exactly what `decode_ban_list` refuses. `crate::classify_post_plaintext` checks this domain first and returns `PostContent::BanList` or `PostContent::MalformedBanList`, never a post.

## Wire layout

```text
"lakko/channel-ban-list/v1\0"(26) ‖ version(1)=0x01 ‖ updated_at_ms(8 BE) ‖ count(2 BE) ‖ count × author_key(32)
```

Length `BAN_LIST_MIN_LEN` (37) to `BAN_LIST_MAX_LEN` (32,805). The NUL-terminated domain cannot start a chat envelope (JSON).

## Failure modes

`WrongDomain` (strict decoder only), `UnknownVersion`, `Truncated`, `TrailingBytes`, `TooManyKeys` (checked before anything is copied), `DuplicateKey`. No panic.

## What this module does NOT do

It hides comments only; reactions cannot be banned. It does not re-issue itself after a rotation (the publisher's client does, with the stored stamp, as for the metadata record). In a private channel a ban is the existing removal.

## Attacker exposure

The plaintext comes from any publisher key (a malicious co-publisher or a leaked key). Fuzz target `channel_ban_list_decode` decodes arbitrary bytes after the domain, asserts bounds, canonical re-encoding and agreement with the classifier; `channel_meta_decode` also covers the classifier's ban-list arm.

## References

ADR-0087 section 13a; `crate::meta`; tests `the_layout_is_pinned_and_round_trips`, `decode_refuses_every_malformed_shape_and_compose_the_same`, `banning_appends_lifts_and_drops_the_oldest_at_the_cap`.
