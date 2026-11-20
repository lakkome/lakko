<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-attest/src/supporters_list.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if supporters_list.rs changes and this file does not. -->

# `supporters_list` — the signed list of backer names (spec)

**Module:** `crates/lakko-attest/src/supporters_list.rs`
**One line:** Sign (offline tool) and verify (clients) the supporters list served at `/.well-known/lakko-supporters`.

## Why this module exists

ADR-0097 section 2: the names backers chose are shown in Settings > About. The file is fetched from a static host the network or an operator could tamper, so it is signed offline by the ADR-0038 release key under its own domain, which the clients already pin.

## Public contract

- `SupportersList { list_version, signed_at_unix_millis, names }`.
- `sign(&SigningKey)` — refuses exactly what `open` refuses: a name breaking the display-name rule (`BadName(index)`), more than `MAX_SUPPORTERS` names (`BadCount`), more than `MAX_SUPPORTERS_LIST_LEN` bytes (`BadLength`).
- `open(bytes, &VerifyingKey)` / `verify_supporters_list` — length bounds, format byte, count bound against both `MAX_SUPPORTERS` and what the bytes can hold (two bytes per name at least), then the Ed25519 signature over `DOMAIN ‖ body`, then each name (in bounds, UTF-8, `lakko_contacts::check_display_name`), then no trailing bytes. Returns the list.

## Wire layout

```
format(1)=0x01 ‖ list_version(8 BE) ‖ signed_at_ms(8 BE) ‖ count(4 BE)
‖ count × (len(1) ‖ UTF-8 name) ‖ sig(64)
signing input = "lakko/supporters-list/v1" ‖ every byte before sig
```

## Invariants upheld

- Domain separation: no other kind in this crate verifies as a list, nor a list as one of them, under a shared key (test `domains_do_not_cross_with_the_other_kinds`).
- One list has one encoding (no trailing bytes; lengths are exact).
- No freshness check: a client keeps the highest `list_version` it verified and ignores a lower one (client logic).

## Failure modes

`BadLength`, `BadVersion`, `BadCount`, `Malformed`, `BadName(i)`, `BadSignature`. No path panics (proptest `open_never_panics`, fuzz target).

## Attacker exposure

The whole file. Size and count are bounded before any allocation for names; the signature is checked before any name is parsed, so a forged file costs one signature check. Fuzz target `crates/lakko-attest/fuzz/fuzz_targets/supporters_list_open.rs`.

## References

- ADR-0097 section 2; ADR-0038 (the release key); ADR-0048 (display-name rule).
- Tests (in-module): `sign_then_open_round_trips_with_the_documented_layout`, `a_wrong_key_or_any_flipped_byte_is_refused`, `a_name_breaking_the_display_rule_is_refused_on_both_sides`, `counts_are_bounded_before_allocation`, `domains_do_not_cross_with_the_other_kinds`, proptests.
