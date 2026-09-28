<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0054: Anti-abuse is decided by the server, not paid for by the phone

- **Status:** Accepted (2026-09-27, the maintainer's call; S1-S5 built and deployed to the test stack 2026-09-28, walked live in part; security pass open)
- **Date:** 2026-09-27
- **Context:** Phase 3 (product hardening)
- **Supersedes:** the per-request proof-of-work gate on `services/discovery`,
  `services/push-gw`, `services/ds` and every `services/auth` route except
  account creation; the client-initiated group wake of Phase 1.20e and
  [ADR-0042](0042-push-opaque-group-routing-id.md) (the `push_gid` derivation
  and routing are unchanged, only who asks for the wake moves)

## Context

Every anonymous or pre-session request to the backend is gated by an Equix
proof of work (`lakko-pow-server::verify_proof`): 34 call sites across four
services on 2026-09-27 (auth 14, discovery 13, push-gw 6, DS 1). The proof is
verified by the server, so a script cannot skip it; what it buys is a CPU price
per request. The walk of [ADR-0053](0053-ds-journal-authorisation.md) measured
what that price is for the people it is meant to protect: a phone without a JIT
- every iPhone, since iOS refuses executable memory to an app - pays several
seconds per request at the production effort, and a knock timed out its own
challenge on the Android emulator before the solver was sped up. An attacker
pays the same price on hardware with a JIT, or on a botnet that does not pay for
its own electricity. The maintainer's objection (2026-09-27), adopted here: **a
price that falls hardest on honest phones is the wrong tool; the cut belongs on
the server, keyed on something the server can check.**

Two findings shape the replacement.

1. **The group wake is a separate client command today.** After publishing to a
   group journal the sender's client calls `POST /push/notify-group` on
   push-gw with the group's `push_gid` (web `ChatClient.notifyPushGroupBestEffort`,
   and the two native twins). push-gw is anonymous by design (invariant #5), so
   anyone who holds a `push_gid` could wake every subscribed device of that group
   as often as the proof of work allowed. The server cannot wake on its own: the
   DS does not know the `push_gid` (ADR-0042 separated them on purpose), and it
   sees only ciphertext, so it cannot tell a message from a typing signal or a
   committer beacon - the client decides, and today wakes only on a message or a
   call frame. The per-device `POST /push/notify` has no caller on any client.
2. **The endpoints fall into four groups by what the server can know.**
   - Already bearer-authenticated (discovery register, identity upload,
     KeyPackage upload, invite / call-invite / group-invite upload): the proof of
     work duplicates a session the server already verifies.
   - Anonymous by design but used by a signed-in person (username and channel
     lookup, OPRF evaluation, invite consume, channel-name register, the DS
     knock, push-gw device register / unregister and group subscribe /
     unsubscribe): a session token would name the account where invariant #5
     forbids it.
   - Pre-session guessing surfaces (login, password reset, TOTP recovery, the
     word vault, recovery-contact OTP, device authorize, device request): the
     guess budget is already a per-account server counter where it matters
     (the TOTP lockout, the vault's 10-strike cap); the proof of work added a few
     seconds to each of ten guesses.
   - Account creation: the one request with no identity of any kind behind it.

## Decision

**Abuse is limited by server-side counters on something the server can verify:
membership (the ADR-0053 group key), a session (the bearer token), or an
anonymous token rationed per account. Proof of work survives in one place only:
a single, deliberately heavy proof when an account is created.**

### 1. A wake is a consequence of a signed publish

- A client that wants the recipients woken adds one header to its signed
  `POST` on a claimed group journal: `x-lakko-wake: <push_gid base64url>;<kind>`,
  `kind` one of `message` / `call`. No header, no wake - a beacon, a typing
  signal, a receipt stay silent exactly as today.
- The DS forwards `(push_gid, message_id, kind)` to push-gw **only after** the
  ADR-0053 proof gate passed and the row was appended, over the internal
  network, authenticated by a shared secret (the transparency append-secret
  pattern). It stores nothing and logs nothing about the header.
- push-gw's `/push/notify-group` leaves the public vhost and binds internally;
  `/push/notify` is deleted (no caller). The clients stop calling push-gw to
  wake.
- The DS caps wakes per group journal per window (a server knob), so a member
  who spams wakes is cut at the server.
- The DS cannot check that the `push_gid` belongs to the journal it arrived on.
  Knowing a `push_gid` requires that group's routing secret, i.e. membership
  there, so a member who wakes group B through a publish to group A could have
  woken B directly; the rule "only a writer may wake" holds per request, not per
  pairing. Stated, accepted.

### 2. Anonymous tokens, rationed per account

- `services/auth` issues tokens to a session: `POST /auth/tokens` (bearer)
  evaluates a batch of blinded elements under an issuance key and counts them
  against the account's quota for the window. The client unblinds and keeps a
  small local pool, refilled ahead of need, so issuance and spending are not
  correlated in time.
- **The quota is per period, never a one-off allowance.** An account may draw up
  to N tokens per window (a server knob); the window rolls over on its own, and
  the client refills its pool in the background with the session it already
  holds. Running out never signs anyone out and never asks for a password: the
  one action that needs a token waits until the window rolls over, with copy
  that says so.
- **Sending and reading messages never spends a token.** A chat is a claimed
  journal, gated by the ADR-0053 group key and the per-group caps of decision 1.
  Tokens are spent only on the anonymous actions listed below - looking someone
  up, knocking on a link, opening an invite, registering a device for pushes -
  which an ordinary day does a handful of times.
- A token is spent once, at one verifier, and carries nothing about the account:
  the verifier learns only that some account the issuer rationed produced it.
  Token context names the consuming service, so each verifier keeps its own
  spent-set for the key's lifetime and a token cannot be spent twice or at a
  second service.
- The construction is the privately verifiable Privacy Pass shape - a VOPRF -
  built on the `voprf` crate already in the tree (no new dependency). The
  issuance key is shared between auth and the verifying services, which are one
  operator; a compromised verifier could mint tokens, which is a quota bypass,
  never a key or content exposure. Keys rotate on a schedule; a verifier accepts
  the current and the previous key.
- Consumers: discovery OPRF evaluation, username lookup, channel lookup,
  channel-name register, invite / call-invite / group-invite consume; the DS
  knock; push-gw device register / unregister and group subscribe / unsubscribe.
  Each consumer may additionally count per token-key epoch to cap bursts.

### 3. Bearer routes and pre-session routes lose the proof of work

- Bearer-authenticated discovery writes are limited per account.
- Login, password reset, TOTP recovery, the word vault, recovery-contact OTP and
  device authorize keep their per-account counters and gain a per-IP backoff.
  Counters are keyed on what the request names (a username hash, a contact
  hash), never on whether the account exists, so the endpoints stay
  enumeration-safe.
- Two surfaces cost more than a guess and get their own limits: sending a
  recovery code by email or SMS costs the operator per message (limited per
  contact hash and per IP), and a device request puts a prompt on the target's
  signed-in devices (limited per target account).

### 4. Account creation pays once, heavily

- `signup_start` keeps a proof of work, at an effort set by measurement on the
  slowest supported phone rather than by guess, and `signup_finish` is bound to
  the start's state instead of carrying a second proof. Nothing after signup
  pays again.
- This is the one rationing point the whole scheme rests on: every account is a
  source of anonymous-token quota, so a free account would make the quota worth
  nothing.
- The solver runs off the main thread and the signup screen shows a state for
  it (a UX spec is part of the work).

## Alternatives

- **Keep the proof of work on every route, tune the effort.** Rejected: the
  price falls hardest on phones without a JIT and lightest on an attacker's
  hardware; no effort is both cheap for an iPhone and costly for a server farm.
- **Session-token rate limits on the anonymous routes.** Rejected for lookup,
  knock and push: the token names the account on every request, materialising
  who looks up whom and who knocks where (invariant #5). Kept for the routes
  that are already bearer-authenticated.
- **The DS stores the `push_gid` at claim time and wakes on every write.**
  Rejected: the DS cannot tell a message from a beacon, so every member's phone
  would wake once a minute per active group, and the DS database would hold the
  `group_id` to `push_gid` link that ADR-0042 kept out of it.
- **Platform attestation at signup (App Attest / Play Integrity).** Considered,
  not chosen: nothing equivalent exists for the web client, and the check passes
  through Apple and Google.
- **Per-IP limits alone.** Rejected as the primary control: a botnet rotates
  addresses and a carrier NAT puts many honest people behind one. Kept as a
  secondary backoff on pre-session routes.
- **A captcha.** Rejected: a third-party service in the signup path and a
  privacy cost for every person to stop a few.

## Consequences

- **Positive:** the wake route leaves the internet, and only a journal writer can
  wake a group; lookup, knock and push stop paying seconds per request on a
  phone; the per-request price moves to where the server can count it.
- **Negative:** creating an account becomes a noticeable wait on a phone without
  a JIT, and a guest who joins a call from a link ([ADR-0032](0032-multi-joiner-call-links.md))
  pays it before joining; the DS briefly holds the `group_id` / `push_gid` pair
  in memory for each woken publish - the operator already sees it through the
  timing and address of today's two consecutive requests, but a DS that logs
  headers would now record it; the issuance key is shared across four services.
- **Neutral:** `lakko-pow-server` stays in auth only and `lakko-pow` on the
  clients stays for signup only; the `/pow/challenge` routes on discovery,
  push-gw and the DS go away.

Invariants: #1 unchanged (the issuer sees an account and a count, which it
already knows); #2 unchanged; #3 untouched (no `lakko-mls` change); #4
unchanged; #5 improved for lookup, knock and push subscription (unlinkable
tokens instead of a price), with the in-memory wake pairing above stated as a
residual.

## Open questions

1. The signup effort and how it is measured: which device is "slowest
   supported", and what wait is acceptable there.
2. Guest accounts from a call link: the same heavy cost, a lighter one leaning on
   the [ADR-0033](0033-call-admission-antispam-throttle.md) per-call throttle,
   or tokens minted with the guest account.
3. Quotas: the window length and tokens per account per window, per-consumer burst caps, the DS wake
   cap, and whether a fresh account starts with a smaller quota.
4. Token key rotation period and the spent-set storage per verifier.

## Plan

- **S1** - wake through the DS: the header, the gated internal forward,
  push-gw's internal notify and the removal of `/push/notify`; all three clients
  move the wake into the publish.
- **S2** - the anonymous-token primitive (issuance, unblinding, verification,
  spent-set), property tests and a fuzz target on the verifier's token parser;
  `services/auth` issuance.
- **S3** - both FFI surfaces; the three clients keep a token pool; the
  consumers switch from proof of work to tokens.
- **S4** - bearer and pre-session routes drop the proof of work for counters;
  signup's single heavy proof and its UX state.
- **S5** - docs reconcile and a `lakko-security` pass (touches auth, discovery,
  push-gw and the DS).

## What S1 and S2 found (2026-09-27)

- **A wake fires only on a claimed journal whose proof verified.** On an
  unclaimed journal the header is still parsed (malformed -> 400, nothing
  appended) and the wake is dropped: "only a writer may wake" needs a proof to
  mean anything.
- **A second cap, per `push_gid`.** The first claim of a journal needs no
  proof, so a holder of a `push_gid` could claim fresh journals and wake
  through each; a per-journal cap alone limits nothing. The cost: such a holder
  can spend that group's wake budget and delay its real wakes by up to one
  refill window. Defaults `LAKKO_DS_WAKE_BURST` 30, `LAKKO_DS_WAKE_PER_MINUTE`
  60, chosen, not measured.
- **The wake header is outside the ADR-0053 proof**, which signs method, path,
  query and body. Whatever terminates TLS in front of the DS (the operator's own
  nginx) could add or change it. Recorded for the S5 security pass.
- **The internal notify body** is `{push_gid (64 lowercase hex), envelope_pointer,
  kind}`, hex because the subscription table and routes already spell it so.
- **Token contexts are per rationed action, not per service** (discovery
  lookup, discovery register, invite consume, DS knock, push register), each
  with its own quota; defaults 100 / 10 / 30 / 20 / 100 per account per daily
  epoch, chosen, not measured. Issuance is for the current epoch only; a
  verifier accepts current and previous.
- **Key consistency is not proven to the client.** Keys are derived
  deterministically, so there is no honest reason two clients see different
  public keys, but a client cannot prove the key auth served it is the one
  everyone else got (the known Privacy Pass key-consistency problem). An
  issuer that served per-user keys could link issuance to spending. Residual.
- **Invite links got shorter in the same pass** (ADR-0028 amendment): the
  discovery ledgers moved their open fetch / consume routes behind the existing
  per-IP rate limit, because a 6-byte id is no longer unguessable on its own.

## What S3 built, Rust and services half (2026-09-28)

Host-tested against a local MariaDB; not deployed, no client moved yet.

- **One gate, one header.** `x-lakko-anon-token: <base64url token, no
  padding>` (`lakko_anon_token::TOKEN_HEADER`). `lakko-anon-token-server::gate`
  reads it, verifies for the route's context, records the spend, and refuses
  with `{"error": code}`: `503 anon-tokens-disabled` when the service has no
  master secret (checked first, so a dormant service never serves a token
  route without a token), `401 anon-token-required` / `-malformed` /
  `-wrong-context` / `-expired` / `-invalid` / `-spent`, `500 internal` on a
  spent-set store error (fail closed). A `401` there never means a lost
  session: none of these routes carries one.
- **Where tokens are spent.** Discovery: `POST /discovery/oprf`
  (`discovery-lookup`; the evaluation is the enumeration step, and a channel
  lookup passes through the same namespace-agnostic OPRF route, so
  `/discovery/lookup` and `/discovery/channel/lookup` carry no token and keep
  the per-IP limiter), `POST /discovery/channel/register`
  (`discovery-register`), the three ledger consumes (`invite-consume`). The DS
  knock (`ds-knock`), spent after the body validates so a malformed knock
  costs nothing. push-gw register, unregister, group subscribe, group
  unsubscribe (`push-register`). A consequence worth stating: a joiner
  opening an invite link must now hold a token, so it signs up before the
  consume.
- **Proof of work is gone from discovery, the DS and push-gw**: the
  `/pow/challenge` routes, the `lakko-pow-server` dependency, the effort knobs
  and the `pow_challenges` tables (a drop migration per service, after the
  byte-identical spent-set migration). The bearer routes lost theirs too and
  are now counted per account by a separate write bucket the same size as
  discovery's per-account lookup bucket (they had no per-account limit
  before).
- **FFI.** Both surfaces expose `AnonTokenRequest(context, epoch, count)` ->
  `issueBodyJson()` / `finalize(evaluations, proof, publicKey)` ->
  `AnonToken { context, epoch, headerValue }`, plus the header name, the
  contexts, the default window, the batch cap, `epochAt` and `isLive`. The
  public key is taken from the key route, never from the issuance response.
- **Found on the way:** a contact-invite consume against an absent invite
  answered `500`, because MariaDB's `INSERT IGNORE` downgrades the foreign-key
  violation to a warning; it now answers the documented `404`.

## What S4 found (2026-09-28)

Built in `services/auth` (`src/limits.rs`, migration `0020`) and host-tested
against a local MariaDB; not deployed, no client moved yet.

- **The per-row lockouts were existence oracles.** The TOTP lockout, the vault
  lockout and the recovery-code attempt cap each answered `429` only when the
  named account had a row (a confirmed authenticator, a vault, a live
  challenge); an unknown account always got `401`. Five wrong guesses then
  told a caller whether the account had that factor. They are replaced by one
  counter per (scope, named value), keyed on a BLAKE3 of the account id or
  contact hash the request names and charged before any lookup, so the sixth
  guess is refused identically for any name. The vault's 10-strike destruction
  cap stays (it answers `401` either way).
- **Signup finish was not bound to anything.** Registration was stateless
  between start and finish and `ServerRegistration::finish` verifies nothing,
  so without its own proof a finish alone could create an account. Start now
  writes a `signup_states` row after its proof verifies; finish must consume it
  in the account-creating transaction (`410` without one, `400` when the
  recovery upload does not match what the start carried). One proof, at most
  one account; guest signup (ADR-0032) uses the same start route.
- **Signup effort** is `LAKKO_SIGNUP_POW_EFFORT_BITS`, default 8 (the previous
  `LAKKO_POW_EFFORT_BITS` default), still to be set by measurement on the
  slowest supported phone (open question 1). The challenge TTL in
  `lakko-pow-server` is 300 s and bounds how heavy that effort can be.
- **Limits and failure mode.** A limit answers `429 {"error", "retry_after_secs"}`
  with `Retry-After`, never `401`. The name counters fail closed (`503`) on a
  store error, on the send routes and the guessing routes alike; nothing fails
  open. Per-address buckets are in memory, so addresses are never stored and
  a restart refills them. Defaults are chosen, not measured (knobs
  `LAKKO_AUTH_LIMIT_*`, `LAKKO_AUTH_IP_LIMIT_*`).
- **A name counter can be spent by someone else.** Anyone who knows an account
  id can use up its login or recovery budget for the window; the per-address
  bucket bounds how fast one address can do it, and each factor has its own
  scope so another recovery route stays open. Stated, accepted.

## S5, login by name (2026-09-28)

Built in `services/auth` (`src/login_handle.rs`, migration `0021`), the core
and both FFI surfaces, host-tested against a local MariaDB; not deployed, no
client moved yet. The maintainer's call: sign in by name directly at auth,
with no discovery lookup before a session.

- **The problem S3 created.** A username lookup (`/discovery/oprf`) spends an
  anonymous token and a token needs a session, so a device with no cached
  account id could no longer find the account to sign into or recover.
- **The handle.** `login_handle = BLAKE3-derive-key("lakko/auth/login-handle/v1",
  canonical_username)`, 32 bytes, where the canonical username is the output
  of the one username rule (normalise, then `[a-z0-9_]{3,32}`). Defined once
  in `lakko_contacts::username::login_handle` (beside the rule; the crate is
  light and every FFI crate already links it), with the normalising entry
  `lakko_discovery_client::login_handle` (which owns NFKC); golden-pinned;
  `loginHandle(username)` on UniFFI and wasm, refusing a non-conforming name
  with the existing typed username errors.
- **Storage.** `login_handles(handle_key PK, account_id UNIQUE, FK cascade)`,
  `handle_key = keyed-BLAKE3(LAKKO_AUTH_HANDLE_PEPPER, handle)`. One handle
  per account; bearer `PUT /auth/login-handle` binds (replacing the
  account's previous handle), `DELETE` removes, a handle another account
  holds is `409 handle-taken` and changes nothing, account deletion removes
  it. Pepper unset: every name route and both handle routes answer
  `503 login-by-name-disabled`, with a loud startup warning.
- **Flag day.** Login start, recovery login start, password reset start,
  TOTP recover, word-vault recover start and finish, recovery-contact
  recover start and finish take `handle` in place of `account_id`. Signup,
  device authorize and redeem keep `account_id`. The account id reaches the
  client only in a finish response, after a successful proof (login,
  recovery login and reset finish; TOTP and recovery-contact recover; the
  vault releases the phrase, whose reset finish then returns it).
- **Enumeration safety.** A handle nobody holds resolves to a stand-in
  account id derived from its `handle_key`, stable per handle, and the
  handler runs its ordinary path on it: the same queries, the dummy verify,
  the vault decoys (now stable per handle). OPAQUE takes its fake-credentials
  path with the credential identifier `stand_in || kind`, so a repeated
  unknown name meets the same OPRF key every time, as a real name does (the
  test compares the OPRF evaluation under a fixed blind; the rest of a
  credential response carries fresh nonces for a real account too). The
  per-name counters of S4 key on the handle, charged before the lookup.
- **Invariants.** Auth now holds a peppered handle to account mapping. The
  operator gains nothing it lacked: discovery's OPRF key already lets it map
  any guessed name to its account, and the handle is itself a public
  function of the name. A database leak without the pepper is not a
  dictionary over usernames. Guest accounts (ADR-0032) have no username and
  so no handle. #1, #2 and #5 unchanged; no `lakko-mls` change.
- **A handle can be squatted.** Neither server knows the name (discovery
  holds an OPRF output, auth a peppered hash), so auth cannot check that the
  account binding a handle owns that name at discovery. A signed-in account
  can bind the handle of a name it does not own: it gains no access to the
  owner's account (OPAQUE still needs the owner's password), but the owner's
  sign-in by name then fails as a wrong password and the owner's bind answers
  `409`. Each squat costs an account (the signup proof). Honest clients bind
  right after registering the username, so the two first-come registries
  agree unless someone races them. Recorded for the security pass; a fix
  needs one first-come registry that holds both, or a binding the name's
  owner can prove.
- **Not moved: `/device/request`.** Another-device recovery needs the account
  id before any session, not only for this route: the pairing payload binds
  it (the scanner-side decode in `lakko-mls` checks it) and the DeviceGroup
  join takes it. Changing the route alone would not make that path work
  from a name, so it keeps `account_id`, and a device with no cached id
  cannot start it. Open, needs a design (for example, the fresh device learns
  the id from the confirming device inside the E2EE channel).
- **Not moved: the passkey allow-list start** (`/webauthn/auth/start` with an
  account id). The web client uses the discoverable path, which names no
  account.
- **Signup cancel.** Both FFI surfaces gained a chunked solver
  (`powSolveChunk` on UniFFI, `PowSolver.solveChunk` on wasm: at most
  `maxAttempts` nonces, none when nothing was found), so a client can stop
  the signup proof of work between chunks.


### Claim order (added 2026-09-28, the coordinator's call on the squatting finding)

The handle and the discovery name live in two first-come registries that
cannot check each other (neither server knows the name). An honest client
therefore claims a name in one order: bind the handle at auth first, then
register at discovery; an auth 409 stops the claim before discovery, and a
discovery "taken" releases the handle again (or re-binds the previous one on a
rename). With every honest client following that order, a squatter can only
take a name nobody has claimed anywhere yet, which is ordinary first-come
naming. The residual: a modified client can still register a name at
discovery without the handle, if nobody holds the name at discovery yet; the
owner of the handle then signs in fine but is not findable under that name.
Recorded for the security pass.

### Another-device recovery on a fresh device (open)

`/device/request` still takes `account_id`, and the pairing payload carries
it and is checked by the confirming device (`lakko-mls` pairing decode). Before
S3 a fresh device learned the id by a username lookup; since S3 that lookup
needs a token, so a device with no cached id cannot start this path. Open for
the maintainer: a pre-session resolve that returns real or stand-in ids (a
stand-in must be indistinguishable from a UUIDv7, including its timestamp,
which is hard), or a pairing change so the confirming device supplies the id
(an `lakko-mls` change, invariant #3).

## References

- [ADR-0042](0042-push-opaque-group-routing-id.md), [ADR-0053](0053-ds-journal-authorisation.md),
  [ADR-0033](0033-call-admission-antispam-throttle.md)
- Privacy Pass (IETF): the architecture and the privately verifiable issuance
  protocol built on a VOPRF
