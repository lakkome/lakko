<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0097: Supporter codes that hold a beta place, and a signed supporters list

- **Status:** Proposed
- **Date:** 2026-11-20
- **Context:** the 1000-account beta cap (ADR-0046 amendment,
  `services/auth/src/account_cap.rs`), anonymous tokens (ADR-0054), servers that cannot
  link accounts (ADR-0078), the signed release notice (ADR-0038), paying for a plan
  without linking the payer (ADR-0094, proposed in another branch)
- **UX:** [`docs/ux/supporters.md`](../ux/supporters.md)

## Context

The Kickstarter campaign (`docs/funding/kickstarter-draft.md`) offers two rewards that
need code: a place in the beta when all places are taken, and a name in a supporters list
inside the app. Kickstarter gives Lakko each backer's email address; Lakko mails the
backer a code. The plain design, "the code is typed at sign-up and spent there", gives
auth a row joining a code, hence an email address, to an account. The maintainer chose
(2026-11-20) a blind exchange instead, the shape ADR-0094 gives paid plans.

## Decision

### 1. Supporter codes

1. **Minting.** An operator command in the auth binary (`lakko-svc-auth supporter-codes
   mint --count N`) draws N codes of 80 random bits, prints them once in Crockford base32
   (16 characters, shown in four groups) and stores only `BLAKE3-keyed(pepper, code)` in
   `supporter_codes`. The codes are mailed to backers by the maintainer, outside Lakko.
2. **The token.** An anonymous token of the ADR-0054 kind (VOPRF, `lakko-anon-token`) in
   a new context `beta-place`, with a window of 30 days (a window of its own for this
   context; the other contexts keep theirs). Auth both evaluates and verifies, as for
   every other context.
3. **Redeeming the code, without a session.** `POST /auth/supporter-code/redeem
   {code, blinded_element}` takes no `Authorization` header. Auth finds the code by its
   keyed hash, then evaluates the one blinded element under the `beta-place` key of the
   current epoch and returns the evaluation with its DLEQ proof. **One code, one
   evaluation:** the client derives the token's nonce and blinding factor
   deterministically from the code and the epoch, so a retry sends the same blinded
   element; auth stores the first evaluation per code and returns it to every later
   request in that epoch, and refuses a code whose stored evaluation is from an earlier
   epoch (the place was held then; a code is good for one window). The client checks the
   proof against the key from `GET /auth/tokens/key/beta-place/{epoch}`, never a key in
   the response. Unknown code: `404 {"error":"supporter-code-unknown"}`. Tries are limited
   per address (in memory) and per code.
4. **Using the place.** The first username bind (`PUT /auth/login-handle`) may carry the
   token in `x-lakko-anon-token`. Auth looks at it only when the cap refuses the bind:
   then a token that verifies and is not in the spent set admits the bind past the cap
   and is spent in the same transaction. A spent token gives
   `403 {"error":"supporter-code-used"}`; any other failure is the ordinary
   `403 registration-closed`. A bind the cap admits anyway leaves the token unspent.
5. **What the separation gives.** `supporter_codes` holds code hashes and evaluations;
   the spent set holds spend ids. Neither joins a code to an account: the blinding hides
   which evaluation a spent token came from. A copy of the database, or a breach of it,
   does not tell which backer has which account.

### 2. The supporters list

1. A static file at `/.well-known/lakko-supporters` (nginx, a separate root as for the
   release notice): `format(1) ‖ list_version(8 BE) ‖ signed_at_ms(8 BE) ‖ count(4 BE) ‖
   count × (len(1) ‖ UTF-8 name) ‖ sig(64)`, Ed25519 under the domain
   `lakko/supporters-list/v1`, a sixth fixed-domain kind in `lakko-attest`. Each name
   follows the display-name rule of ADR-0048 (1-64 characters, at most 255 bytes, no line
   break, no control or format character); at most 100 000 names and 2 MiB in all
   (chosen, not measured).
2. Signed offline by the release publisher key of ADR-0038, under its own domain, so the
   clients' existing release-key pin verifies it; nothing new to pin. Signed with the
   existing `tools/release-manifest` binary (a `supporters` subcommand) from a plain text
   source file, one name per line.
3. Clients fetch it as `docs/ux/supporters.md` section 2 says, verify it on both FFI
   surfaces (`verifySupportersList`), keep the highest `list_version`, and show it. A
   build with no release key pinned shows no list.

## Consequences

- **Not hidden from the operator watching live.** Redeeming a code and signing up happen
  minutes apart from one address. An operator who logs addresses and times can join them,
  as ADR-0094 states for purchases. The promise is that the stored data does not join
  them.
- **The maintainer knows who got which code** (the mailing list from Kickstarter). The
  blind exchange is what keeps that list from meeting an account.
- **A code mailed to the wrong person or shared** holds one place for whoever redeems it
  first; the second account to use the same token is refused as `supporter-code-used`.
- **A held place lasts one 30-day window plus the previous one** (the verifier accepts the
  current and the previous epoch). A code not used for sign-up in that time is spent for
  good: the stored evaluation belongs to the old epoch. The UX states 30 days.
- **Context numbering.** `beta-place` takes the next free context byte in this branch; the
  plans branch (ADR-0093, ADR-0094) appends contexts too. Whichever merges second
  renumbers, before any release.
- **Invariants.** #1: no email, no name of a backer on the server; the supporters list is
  public by the backer's choice. #2: unchanged. #5: unchanged; the token is the ADR-0054
  primitive.

## As built (2026-11-20, server and core; clients not yet)

Built in `lakko-anon-token`, `lakko-anon-token-server`, `lakko-attest`, `services/auth`,
`tools/release-manifest` and both FFI surfaces; host-tested only, nothing deployed or
walked. Where the build differs from or adds to the text above:

- **Context and window.** `beta-place` is wire byte 16, last. Its 30-day window is
  `TokenContext::fixed_window_secs` / `window_secs(configured)` in the core, used by
  `verify_token`, by the key route and by the redeem route; the configured window of every
  other context is unchanged. `POST /auth/tokens` refuses it with
  `400 context-not-issuable` (and its quota row is 0). The spent-set pruner now prunes per
  context on each context's own epoch scale (`prune_spent` skips fixed-window contexts,
  `prune_spent_fixed` handles them); before, a daily prune would have deleted every spent
  `beta-place` row at once. `anonTokenContexts()` on both FFI surfaces lists only the
  session-issuable contexts, so client token pools never ask for `beta-place`.
- **Deterministic request.** `SupporterCode::token_request(epoch)`: nonce
  `BLAKE3-derive-key("lakko/supporter-code/nonce/v1", code ‖ epoch_be)`; the blinding scalar
  is RFC 9497's ordinary random scalar drawn from a BLAKE3-XOF stream keyed by
  `BLAKE3-derive-key("lakko/supporter-code/blind/v1", code ‖ epoch_be)` (64 bytes reduced
  modulo the group order). No `voprf` `danger` feature and no new crate.
- **Redeem body carries the epoch:** `{code, epoch, blinded_element}`. The token input
  contains the epoch, so client and server must agree on it; another epoch is
  `400 epoch-not-issuable` with `current_epoch`, as on `POST /auth/tokens`. Response
  `{context, epoch, evaluation, proof}`. Other codes: `400 supporter-code-malformed`,
  `400 bad-blinded-element`, `404 supporter-code-unknown`, `409 supporter-code-mismatch`
  (redeemed this epoch over another element), `410 supporter-code-expired` (redeemed in an
  earlier window), `429 supporter-code-limited` (address bucket
  `LAKKO_AUTH_IP_LIMIT_SUPPORTER_CODE`, default 20/3600, and per-code counter
  `LAKKO_AUTH_LIMIT_SUPPORTER_CODE`, scope 23, default 10/3600; both chosen, not
  measured), `503 supporter-codes-disabled` (no pepper or no anonymous-token master).
  `supporter_codes` (migration `0040`) also stores the blinded element, so a repeat is
  recognised by equality.
- **Pepper:** `LAKKO_AUTH_SUPPORTER_CODE_PEPPER`; the code hash is
  `BLAKE3-keyed(pepper, 10 code bytes)`.
- **Stated cost, added:** whoever holds a code can recompute its token and so its spend
  identifier. The maintainer's code list together with a copy of auth's database tells
  which codes were spent, still not by which account.
- **Supporters list.** Names are checked with `lakko_contacts::check_display_name`, so
  `lakko-attest` now depends on `lakko-contacts` (no cycle; nothing new in the tree). The
  tool subcommand is `supporters --version N --names <file> [--seed <file>] [--out <file>]`;
  `just sign-supporters <version> <names>`.

## Alternatives

1. **Code spent at the bind.** Simplest; joins the backer's email to the account in
   auth's database. Rejected by the maintainer.
2. **Publicly verifiable blind signatures (RFC 9474).** Would let a service other than
   auth verify; nothing but auth verifies here. Rejected as for ADR-0094.
3. **The list built into each release.** Nothing fetched, but every addition needs a
   release on four platforms. Rejected by the maintainer.

## References

- `services/auth/src/account_cap.rs`, `services/auth/src/anon_tokens.rs`
- `crates/lakko-anon-token`, `crates/lakko-attest/src/release_manifest.rs`
- ADR-0038, ADR-0046, ADR-0048, ADR-0054, ADR-0094
