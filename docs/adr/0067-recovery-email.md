<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0067: A recovery email: a new password by a mailed code

- **Status:** Proposed (2026-10-12). The maintainer decided the direction
  the same day (D1 to D4 below); the rest is this document's proposal.
  Nothing is built.
- **Date:** 2026-10-12
- **Context:** account recovery, Part 6 (after
  [ADR-0064](0064-recovery-model-simplification.md),
  [ADR-0065](0065-account-key-reset.md) and
  [ADR-0066](0066-account-takeover-resistance.md))
- **Amends (when built):** [ADR-0064](0064-recovery-model-simplification.md)
  (it removed the email factor; this brings a narrower one back),
  [ADR-0018](0018-recovery-factor-cascade.md) (the list of server factors).
- **UX spec:** [`docs/ux/identity-recovery.md`](../ux/identity-recovery.md)
  section 19.

## Context

A person who forgets the password on a device that still holds the account
has three ways back today: a passkey, three words of the phrase (checked by
the device), an authenticator code. A person who never wrote the phrase
down, has no passkey and no authenticator has none. The phrase is hard for
ordinary people, and the maintainer wants a route they already understand.

ADR-0064 removed the email one-time code for two reasons: sending a code
makes the server see the address next to the account, and a mailed code can
only return access, never keys. Both still hold. The maintainer's decision
is that the first is acceptable when the server **stores** nothing that
names the address, and that the second is acceptable because the route is
for the password only.

A third option, mail as a notification channel ("your password was
changed"), was rejected by the maintainer: it requires the server to hold
the address at all times.

## Decision

### Maintainer decisions (2026-10-12)

- **D1.** The stored value is derived from the **username and the address
  together**, so one address on two accounts gives two unrelated values and
  the server cannot group accounts by address or answer "is this address
  registered".
- **D2.** A mailed code changes the **password only**. Resetting the account
  keys stays the separate action of ADR-0065.
- **D3.** When the account has TOTP, its code is required as well.
- **D4.** The recovery phrase stays as it is.

### 1. What the server stores

One row per account, in `recovery_emails` (a new migration):

```
account_id   BINARY(16) PRIMARY KEY  (FK accounts, cascade)
email_key    BINARY(32)
created_at   TIMESTAMP
```

`email_key = keyed-BLAKE3(LAKKO_AUTH_EMAIL_PEPPER,
"lakko/auth/recovery-email/v1" ‖ login_handle(32) ‖ len(2 BE) ‖ address)`

- `login_handle` is the ADR-0054 S5 value the client derives from the
  canonical username. The server never sees the username itself, as today.
- `address` is the normalised address: trimmed, lower-cased as a whole,
  exactly one `@`, 3 to 254 bytes, no control characters. No provider rules
  (dots, plus tags) are applied.
- The pepper is an environment secret, separate from the handle pepper.
  Unset: every route of this ADR answers `503 recovery-email-disabled`, and
  an anonymous `GET /recovery-email/available` answers `{available: false}`,
  so the clients hide the rows.

Invariant #1 allows "a hash for recovery" and forbids storing the address,
encrypted or not. The row satisfies it. An address has little entropy, so
with the database **and** the pepper an attacker who already suspects a
(username, address) pair can confirm it; without the pepper they cannot.
That is the same class as the login-handle table.

The address is in server memory from the request until the letter is handed
to the mail relay. It is never logged; the mail relay's own log is outside
Lakko's control and is stated as a cost.

### 2. Codes

`email_challenges`:

```
id           BINARY(16) PRIMARY KEY (random)
account_id   BINARY(16)            (a real account, or a stand-in)
purpose      TINYINT               (1 enrol, 2 recover)
code_key     BINARY(32)            keyed-BLAKE3(pepper, purpose ‖ id ‖ code)
pending_key  BINARY(32) NULL       (enrol: the email_key being confirmed)
attempts     TINYINT
expires_at   TIMESTAMP             (10 minutes)
```

Six decimal digits from the CSPRNG by rejection sampling. Five tries per
challenge, then the row is deleted. One live challenge per (account,
purpose); a new one replaces the old. A pruner sweeps expired rows.

### 3. Enrol, replace, remove (bearer)

Each needs the fresh-auth grant of ADR-0065 (`403 fresh-auth-required`
otherwise), like adding a passkey: a session alone must not be able to add
a way to change the password.

- `POST /recovery-email/enrol/start {fresh_grant, handle, address}` checks
  that `handle` is the account's bound login handle, mints a challenge
  holding the prospective `email_key`, sends the letter, answers
  `{challenge_id, resend_after_secs}`.
- `POST /recovery-email/enrol/finish {challenge_id, code}` upserts the row.
  A replace is the same pair; the old key is overwritten here.
- `DELETE /recovery-email {fresh_grant}`.
- `GET /recovery-email` answers `{enabled: bool}` for the session's account.
- Unbinding or re-binding the login handle deletes the row in the same
  transaction (the key was made from the old handle).

### 4. Recover (anonymous)

- `POST /recovery-email/recover/start {handle, address}` always answers
  `{challenge_id, resend_after_secs}`. The handle resolves as in ADR-0054
  S5 (an unknown handle resolves to a stand-in account). The server computes
  the key, compares it in constant time with the stored one (or with a
  pepper-derived dummy) and writes a challenge either way; the letter is
  queued only on a match. Sending is asynchronous so a match and a miss
  answer in the same time.
- `POST /recovery-email/recover/verify {challenge_id, code}` answers
  `{ticket, totp_required}` or the one uniform `401 code-refused` (wrong,
  expired, used up, never sent). A challenge of a stand-in can never verify.
- `POST /recovery-email/recover/totp {ticket, code}` when required; the
  existing TOTP verify with its replay and window rules.
- `POST /recovery-email/recover/password/{start,finish}` run the OPAQUE
  registration for `KIND_PASSWORD` under the ticket. `finish`, in one
  transaction under the account row lock: ADR-0066's `capture_anchor`
  (before the envelope changes), replace the password envelope, delete
  in-flight login states, write the account notice of section 5, consume
  the ticket, answer the session pair.

The ticket is a random 32-byte value stored hashed in `email_challenges`
(the verified challenge row is rewritten into it), valid 10 minutes, bound
to the account, single use, with a flag for "TOTP passed".

`finish` mints **no** fresh-auth grant. The person holds the new password
and a password login mints one as it always does; see "Stated costs".

Sessions of the account's other devices are not touched: the keys did not
change, and a signed-in device of the owner is what shows the notice of
section 5.

### 5. Telling the account's own devices

The acting device may hold no keys, so the notice cannot ride the account's
DeviceGroup like the "device was added" notice does. A small server table:

```
account_notices(id, account_id, kind TINYINT, at TIMESTAMP)
kinds: 1 password changed by email, 2 recovery email added,
       3 replaced, 4 removed
```

`GET /account/notices?after=<id>` (bearer) returns the rows; clients fetch
at session landing and keep a per-device "seen" cursor locally. Rows are
pruned after 60 days. A row carries no address and no device detail.

### 6. Limits

New `Scope` numbers (6 to 8 stay retired): letters per login handle
(recover start), letters per account (enrol start), verify attempts per
handle, plus the per-address `Category::Attempt` bucket. Defaults are to be
chosen and marked as chosen, not measured. A limit answers the existing
`429` shape. Counters are charged before any lookup, as elsewhere.

### 7. Sending

A `MailSender` trait with a log-only default and an SMTP implementation
(`lettre`, which ADR-0064 removed from the tree; it returns as a dependency
of `services/auth` only). Configuration by environment; unset means the
feature is off (section 1). The letter is fixed text plus the code, in the
language tag the client sends (`Accept-Language`, matched against the
supported set, English otherwise); the templates live in the l10n catalog
and are compiled into the service.

### 8. Clients

Section 19 of the UX spec: a Settings row, an optional sign-up step, one
more row in **Get back in**, four steps, and the notices. No core crate
changes: the login handle and the OPAQUE client already exist on both FFI
surfaces.

## Stated costs

- **The mailbox becomes a way to the password.** Whoever reads it and knows
  the username chooses a new password. On a device without keys they then
  hold a session and a password, which ADR-0065 accepts as proof for a key
  reset. So the mailbox reaches a reset in two steps. What stands against
  it: TOTP when enabled (D3), the notice on the owner's devices, and
  ADR-0066's recall, whose anchor is captured before the password changes
  so the owner's old device takes the account back with the **old**
  password for 30 days.
- **Open, for the maintainer:** refuse a password that was set by this
  route as proof for a key reset for some period. It is one rule in
  `require_fresh_grant` (a timestamp on the account). The maintainer has
  rejected delays elsewhere; this document does not include one.
- The server sees (handle, address) for the length of a request, and the
  mail relay sees the address and that Lakko wrote to it.
- A person who changes their username must add the address again.
- No way to learn the address back from the account: a person who forgets
  which address they used has to try them.

## Invariants

- **#1** holds as worded: a hash for recovery, the address never stored.
- **#2** untouched: the route touches the password envelope only, never key
  material.
- **#3** untouched: no `lakko-mls` change.
- **#4, #5, #6, #7** untouched. Recovery by this route changes no identity
  key, so contacts see nothing.

## Alternatives considered

- **A recovery file sealed by the password and mailed by the person to
  themselves.** Keeps the server out entirely; the maintainer rejected it as
  unusable for ordinary people.
- **Mail as a notification channel.** Rejected by the maintainer: the server
  would hold the address at all times.
- **A hash of the address alone.** Lets the server (or a leak plus the
  pepper) test an address against every account at once and link accounts
  that share one. D1 binds it to the username instead.
- **The code also resets the keys.** One step instead of two for a person
  who lost everything, and for an attacker. D2 keeps them apart.
- **Hashing on the client.** The server must see the address to send the
  letter, so it would hide nothing.

## Sprint plan

1. `services/auth`: migrations, the two tables and the notices table, the
   routes, limits, the sender trait with the log-only sender, DB-gated
   tests (enumeration: a match and a miss are indistinguishable in body and
   status; the anchor is captured; TOTP cannot be skipped).
2. SMTP sender and letter templates; infra configuration.
3. Web, iOS and Android per UX section 19, in one pass.
4. `lakko-security` on request (it touches auth and a new anonymous route).

## References

- ADR-0054 (login handle, stand-in accounts, limits), ADR-0064, ADR-0065
  (fresh-auth grant), ADR-0066 (recall anchor).
