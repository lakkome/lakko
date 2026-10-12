<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0067: A recovery email, an account without a saved phrase, and taking an account back with the phrase

- **Status:** Accepted (2026-10-12; the maintainer decided D1 to D8 below,
  including the exception to security invariant #2, and asked for it to be
  built). Core, `services/auth` and the three clients are built and
  unit-tested, see "As built"; nothing was walked on a device, against a
  server or through a mail relay.
- **Date:** 2026-10-12
- **Context:** account recovery, Part 6 (after
  [ADR-0064](0064-recovery-model-simplification.md),
  [ADR-0065](0065-account-key-reset.md) and
  [ADR-0066](0066-account-takeover-resistance.md))
- **Amends (when built):** [ADR-0064](0064-recovery-model-simplification.md)
  (the phrase is no longer shown at sign-up; an email route returns),
  [ADR-0018](0018-recovery-factor-cascade.md) (the list of server factors),
  [ADR-0066](0066-account-takeover-resistance.md) (recall gains a phrase
  arm), and **security invariant #2** (one stated exception, section 2).
- **UX spec:** [`docs/ux/identity-recovery.md`](../ux/identity-recovery.md)
  section 19.

## Context

The recovery phrase is hard for ordinary people. Today a person who did not
write it down, has no passkey and no authenticator cannot get back in after
forgetting the password, and loses the account with its username after
losing the only phone. ADR-0064 removed the email one-time code because the
server sees the address when it sends a letter and a mailed code cannot
bring keys.

What a restore brings today, verified in `lakko-backup::BackupPayload`: the
identity seed, the contacts, the MLS group state and the synced state
(folders, pins, read positions). It does not hold message text; past
messages come only from another device.

## Maintainer decisions (2026-10-12)

- **D1.** The server keeps a value derived from the **username and the
  address together**, never the address.
- **D2.** The phrase is **not shown at sign-up**. An account starts without
  a saved phrase and the person may save one later in Settings.
- **D3.** While **no phrase is saved**, a mailed code restores **full
  access**: the same keys, contacts and chats. The person is told plainly
  that this protection is only as strong as the mailbox.
- **D4.** The exception to invariant #2 that D3 requires is accepted
  (section 2).
- **D5.** Once **a phrase is saved**, a mailed code changes the password
  only; a key reset remains possible afterwards as its own action.
- **D6.** A person who holds the phrase can **take the account back for a
  limited time** after such a reset.
- **D7.** Adding an email may be skipped, with a warning.
- **D8.** When the account has an authenticator, its code is required in
  addition to the mailed code, in both modes.

## Decision

### 1. Two modes of an account

| | **Mail-protected** (no phrase saved) | **Phrase-protected** (phrase saved) |
|---|---|---|
| Mailed code gives | new password and the keys | new password only |
| Keys on a new device | another device, passkey with keys, the mailed code | another device, passkey with keys, the phrase |
| Server holds | `email_key` and an **email vault** | `email_key` only (if an email was added) |
| After a hostile reset | ADR-0066 recall from an old device | that, or recall with the phrase from any device |

A third state exists: no phrase saved and no email added. Such an account
rests on the password and the devices that hold it; the person was warned
(D7).

The device still generates the 12-word phrase at sign-up and derives the
identity from it (ADR-0017 is unchanged); it just does not show it.

### 2. The email vault, and the exception to invariant #2

In mail-protected mode the client seals the 16-byte phrase entropy and the
server stores it:

```
vault_key = Argon2id(login_handle(32) ‖ len ‖ normalised address,
                     salt = "lakko/email-vault/v1" ‖ vault_salt(16))
sealed    = lakko_mnemonic::vault seal (44 bytes, as the passkey vault)
```

`email_vaults(account_id PK, vault_salt, sealed_entropy)`, wrapped again at
rest under `LAKKO_VAULT_ENC_KEY`. The server releases `(vault_salt,
sealed_entropy)` only in the answer to a verified mailed code (and TOTP
code, D8). The client re-derives the key from the username and address the
person typed, opens the vault, rebuilds the phrase and runs the **existing**
phrase restore: the fail-closed bundle check, the recovery login, the backup
restore and "Choose a new password". Nothing downstream is new.

**The exception, stated as it is.** The server handles the address in the
clear when it sends a letter, at enrolment and at every recovery, and it
holds the salt and the blob. An operator who records the address can open
the vault and derive the account's keys. So for a mail-protected account
invariant #2 ("the server only sees ciphertext and public artefacts; no
root keys") does not hold against the operator, nor against anyone who
controls the mailbox and knows the username. It still holds against a copy
of the database taken without the addresses (the attacker must guess the
address per account through Argon2id and, for a live database, unwrap the
at-rest key).

Proposed wording for invariant #2, for the maintainer to apply to
`CLAUDE.md`: "Exception (ADR-0067): for an account whose owner has not saved
a recovery phrase, the server holds the phrase entropy sealed under a key
derived from the username and the recovery email address; the operator, who
sees that address when sending mail, can open it. Saving a phrase ends the
exception for that account."

### 3. Saving a phrase ends the exception by changing the keys

Deleting the vault is not enough: a copy taken earlier still opens the same
phrase. So "Save a recovery phrase" in Settings is the ADR-0066 key
**rotation** (a new phrase and identity, the other devices kept, contacts
see nothing), followed by deleting the vault. The words shown are the new
ones. From then on the account is phrase-protected. Needs the fresh-auth
grant, as rotation does.

Going back (discarding the phrase to return to mail-protected mode) is not
offered.

### 4. What the server stores for the address

`recovery_emails(account_id PK, email_key BINARY(32), created_at)` with

`email_key = keyed-BLAKE3(LAKKO_AUTH_EMAIL_PEPPER,
"lakko/auth/recovery-email/v1" ‖ login_handle(32) ‖ len(2 BE) ‖ address)`

The address is normalised (trimmed, lower-cased as a whole, one `@`, 3 to
254 bytes, no control characters or inner white space, a non-empty part on
each side of the `@`; no provider rules). It is never stored or
logged. With the database and the pepper, a suspected (username, address)
pair can be confirmed; without the pepper it cannot. Pepper unset: every
route here answers `503 recovery-email-disabled` and an anonymous
`GET /recovery-email/available` answers `{available: false}`.

### 5. Codes

`email_challenges(id, account_id, purpose, code_key, pending_key NULL,
attempts, expires_at)`: six digits by rejection sampling, stored as a keyed
hash, 10 minutes, five tries, one live challenge per (account, purpose).

### 6. Enrol, replace, remove (bearer, fresh-auth grant)

- `POST /recovery-email/enrol/start {fresh_grant, handle, address}` ->
  `{challenge_id, vault_salt, resend_after_secs}` (the handle must be the
  account's bound handle).
- `POST /recovery-email/enrol/finish {challenge_id, code,
  sealed_entropy?}` upserts `email_key` and, in mail-protected mode, the
  vault. The client seals after start, so the vault is bound to the address
  being confirmed.
- `DELETE /recovery-email {fresh_grant}` removes both rows.
- `GET /recovery-email` -> `{enabled, vault}` for the session's account.
- Re-binding the login handle deletes both rows in the same transaction.

Sign-up counts as fresh auth, so the email step right after it asks for
nothing more.

### 7. Recover (anonymous)

- `POST /recovery-email/recover/start {handle, address}` always answers
  `{challenge_id, resend_after_secs}`; an unknown handle resolves to a
  stand-in (ADR-0054 S5); the letter is queued only on a match and sent
  asynchronously, so a match and a miss answer alike.
- `POST /recovery-email/recover/verify {challenge_id, code}` -> `{ticket,
  totp_required}` or one uniform `401 code-refused`.
- `POST /recovery-email/recover/totp {ticket, code}` when required.
- `POST /recovery-email/recover/open {ticket}` -> either
  `{mode: "vault", vault_salt, sealed_entropy}` (the client continues with
  the phrase restore; the ticket is spent) or `{mode: "password"}`.
- `POST /recovery-email/recover/password/{start,finish}` (password mode
  only): OPAQUE registration for `KIND_PASSWORD` under the ticket; `finish`
  in one transaction under the account row lock captures the ADR-0066 recall
  anchor, replaces the password envelope, writes the account notice and
  answers the session pair. It mints no fresh-auth grant.

In vault mode the password is changed by the existing `/password/reset/*`
on the recovered recovery credential, which already captures the anchor.

### 8. Taking the account back with the phrase (D6)

ADR-0066's recall needs an old device (for the old seed) and the old
password (checked against the anchored envelope). A person who holds the
phrase has the old seed without a device and may not know the old password.
So:

- The recall anchor also keeps the **recovery envelope** in force at
  capture (it is derived from the phrase).
- `POST /account/recall/{status,start,finish}` accept a second proof: the
  statement signed by the anchor key, as today, plus an OPAQUE login
  against the anchored **recovery** envelope instead of the password one.
- A device with no cached account names it by login handle on these
  routes; the answer stays the one uniform `recall-refused`.
- The window is the anchor's, 30 days. The outcome is the existing recall:
  the reset transaction with a new password, TOTP and passkeys cleared.

This arm exists only for a phrase-protected account. In mail-protected mode
whoever recovers by mail holds the same phrase as the owner, so the phrase
proves nothing; there the owner has ADR-0066's device recall only.

### 9. Telling the account's own devices

`account_notices(id, account_id, kind, at)`, kinds: password changed by
email, account restored by email, recovery email added / replaced /
removed. `GET /account/notices?after=<id>` (bearer), fetched at session
landing, a local seen cursor, pruned after 60 days. No address, no device
detail. A server table because the acting device may hold no keys.

### 10. Limits, sending

New `Scope` numbers (6 to 8 stay retired): letters per login handle,
letters per account, verify attempts per handle, vault opens per account,
plus the per-address bucket. Defaults chosen, not measured. A `MailSender`
trait with a log-only default and an SMTP sender (`lettre` returns as a
dependency of `services/auth` only); letter text from the l10n catalog in
the requesting device's language.

### 11. Clients

UX section 19: sign-up without the phrase, the "Protect your account" step,
the Settings rows, "Save a recovery phrase", the email row in **Get back
in**, the notices, and the phrase arm of "take the account back". The
three-word row is shown only on a device of a phrase-protected account.
Core: a seal / open pair for the email vault in `lakko-mnemonic` (the
Argon2id derivation, parameters to be measured), on both FFI surfaces.

## Stated costs

- **A mail-protected account is as safe as its mailbox and as its
  operator.** Whoever reads the mailbox and knows the username becomes the
  owner, with the keys, and reads what is sent to the account from then on.
  The owner's signed-in devices show a notice; from there the owner can
  choose a new password, remove the email and save a phrase (which changes
  the keys). It is a race between two parties holding the same keys, and
  the document does not claim otherwise.
- **A phrase-protected account loses the password to the mailbox**, and
  through it a key reset in two steps. TOTP, the notice, device recall and
  now phrase recall stand against it.
- Saving a phrase depends on key rotation (ADR-0066 T5/T6), which is built
  and unit-tested but has never been walked.
- No phrase and no email: a forgotten password with no passkey or
  authenticator cannot be recovered, and a lost only phone loses the
  account.
- An authenticator on the lost phone blocks the mail route (D8). The
  enrolment screens say so.
- Changing the username drops the email and the vault. A forgotten address
  or username cannot be looked up.
- The server sees (handle, address) for the length of a request; the mail
  relay sees the address.

## Invariants

- **#1** holds: a hash for recovery, the address never stored.
- **#2** gains the exception of section 2, limited to accounts without a
  saved phrase.
- **#3** untouched (no `lakko-mls` change). **#6** holds: saving a phrase
  is a proven rotation.
- **#4, #5, #7** untouched.

A gap entry is to be minted in `docs/security/known-gaps.md` when this is
built, naming the exception and its two adversaries.

## Alternatives considered

- **A recovery file the person mails to themselves.** No server trust; the
  maintainer rejected it as unusable.
- **Mail as a notification channel.** Rejected: the server would hold the
  address.
- **A mailed code that only resets the keys for everyone.** No exception to
  #2, but a person without a phrase loses contacts, folders and group state
  on every lost phone; the maintainer rejected that.
- **Sealing the vault under the password as well.** The route exists for a
  forgotten password.
- **An OPRF-derived vault key.** Rate-limits guessing, but the operator
  evaluates its own OPRF and sees the address, so it does not remove the
  exception.
- **Keeping the same keys when a phrase is saved.** Leaves every earlier
  copy of the vault valid.

## Sprint plan

1. Core: the email-vault seal / open in `lakko-mnemonic`, FFI, fuzz of the
   open path; Argon2id parameters measured on the three platforms.
2. `services/auth`: migrations, enrol / recover / open / password routes,
   notices, limits, log-only sender; the recall anchor's recovery envelope
   and the phrase arm; DB-gated tests (a match and a miss are
   indistinguishable; the vault leaves only after the code and TOTP; the
   anchor is captured).
3. SMTP sender, letter templates, infra configuration.
4. Web, iOS and Android per UX section 19, in one pass, including sign-up
   without the phrase and "Save a recovery phrase" over rotation.
5. `lakko-security` on request; the invariant wording and the gap entry.

## As built (2026-10-12): core and server

Host-tested only: `lakko-mnemonic` 43 tests, `lakko-ffi-uniffi` 338,
`lakko-ffi-wasm` 228, `lakko-svc-auth` 188 + 2 against the local MariaDB. No
letter was ever sent through a real relay (`SmtpSender` only compiles); no
fuzzer was run; bindings and native libraries were not rebuilt by these
sprints.

**Core** (`lakko-mnemonic`): `recovery_email::normalize_recovery_email` (the
one address rule, used by the vault and by auth) and
`vault::{seal,open}_entropy_under_email`; on UniFFI and wasm as
`normalizeRecoveryEmail`, `emailVaultSeal`, `emailVaultOpen`; fuzz target
`email_vault_open` (skips Argon2id). Differs from section 2: an HKDF step
with its own `info` sits between Argon2id and the seal, so a passkey blob
and an email blob never open each other; the address rule also refuses
inner white space and an empty part before or after `@`; Argon2id runs at
the backup's parameter set (64 MiB, t=3, p=4), not measured for this use and
not carried in the blob.

**Server** (`services/auth`, migration `0033`): the routes of sections 6, 7
and 9, the recall phrase arm, `lettre` 0.11 for SMTP (rustls), scopes 17 to
20. Differs from the text above:

- The feature needs `LAKKO_AUTH_EMAIL_PEPPER`, `LAKKO_VAULT_ENC_KEY` and
  `LAKKO_AUTH_HANDLE_PEPPER`.
- `accounts.phrase_saved` records the mode, since a second device cannot
  learn it otherwise. It is set by `phrase_saved: true` on
  `POST /account/keys/rotate/finish` (a fresh-auth grant does not survive
  the rotation, so a separate call could not carry one) or by
  `DELETE /recovery-email/vault`.
- A ticket table (`email_tickets`) rather than a rewritten challenge row.
- A key rotation and a key reset delete the email vault (it seals the old
  phrase); a reset keeps `recovery_emails`, a recall deletes it too. **A
  mail-protected account therefore has no vault after a reset until the
  person types the address again**, because no device keeps the address.
- The recovery letter counter per account, when spent, sends nothing and
  answers as usual (a `429` there would reveal a match).
- `recover/verify` charges its per-handle counter after the challenge
  lookup (it carries no handle).
- Recall by handle: the statement carries the first 16 bytes of the handle
  in its account field (the statement format was not changed).
- Letters are English only; `lang` is accepted and ignored.

**Accepted cost (maintainer, 2026-10-12).** ADR-0064 keeps the phrase on
every device of the account, wrapped under the device's hardware-backed key.
With the phrase arm, the old seed alone recalls a phrase-protected account,
so ADR-0066's rule that a device's stored keys without the password are not
enough no longer holds for these accounts. Reaching the seed needs code
running as the app on an unlocked device (the storage cannot be decrypted
elsewhere, and the app never shows the phrase without the password). The
maintainer accepted this; an extra mailed code was considered and not added,
since it would do nothing for an account with no email.

## As built (2026-10-12): the three clients

Unit tests only: web vitest 4921, iOS `LakkoTests` 3430 (0 failures, 2
skipped), Android 3619 on the host JVM (the native `.so` was not rebuilt, so
the vault seal / open, OPAQUE and the rotation are compiled there, never
executed). The server suite is 190 + 2 after two additions the clients
needed: `totp` on `GET /recovery-email` and `since_ms` on
`GET /account/notices`. Nothing was walked; no passkey ceremony ran; the new
routes are exercised on iOS and web through scripted ports, not against a
stubbed server.

One behaviour on the three clients:

- Sign-up shows no phrase. The step after it has three rows, email, passkey
  (added the same day at the maintainer's request, where the device can use
  passkeys) and phrase; a confirmed email or a created passkey returns to the
  step with the row marked done, `Continue` replaces `Not now` once a row is
  done, a saved phrase ends the step. A guest account (call-link join) sees
  neither the phrase nor the step.
- `accountProtection(enabled, vault, phraseSaved)`: phrase, else email when
  `enabled && vault`, else none; "add your email again" when
  `enabled && !vault && !phraseSaved`. The no-way-back banner is not shown to
  an account with a passkey.
- The email flow: start, code, authenticator when required, new password,
  and only then `recover/open`; vault mode runs the existing phrase restore,
  password mode the ticket routes. A vault that does not open shows one line
  and returns to the list. Vault calls run off the main thread.
- Saving a phrase is the ADR-0066 rotation with no device removed and
  `phrase_saved: true`; the words appear after the commit.
- **New words are shown only to a phrase-protected account.** After a
  remove-and-change-keys rotation, a reset or a recall, an account without a
  saved phrase sees no words and is asked to add its email again.
- The three-word surfaces and `Show phrase` exist only for a
  phrase-protected account. Restore your keys hides the phrase row for an
  account without one and offers `Email code` when a vault exists.
- Notices: fetched at each landing; a device with no cursor asks for the
  last 30 days; a device's own action is suppressed by a per-kind marker
  for 10 minutes; `It was not me` opens one screen where the password can be
  changed without another proof for ten minutes (reached from a notice
  only), with remove-email and save-phrase under it.
- Take back with the phrase on the 18.4 form and from Get back in.

Not built, or different:

- The username-change warning and a button behind "Choose a username
  first": no client has a username-change or claim-username screen.
- A reset started while the account's mode cannot be read (offline) treats
  the account as not phrase-protected and shows no words there; they stay
  on the device and the flag corrects at the next landing.
- The device-less phrase recall does not replay an unanswered `finish` on
  Android.
- Android: the done mark is text in the row title; the email row of
  Restore your keys uses another icon; the wordless publish-owed screen's
  title and second paragraph are stand-in strings.
- Web: the passkey screen opened from the step still carries the sign-up
  era title; the passkey list is read once when the step opens.
- No component-level tests of the new screens on any client; the pure rules
  and step machines are tested.
- Translations: the 85 new catalog keys exist in English and Russian
  (machine); the other nine languages show English.

## References

ADR-0017, ADR-0049 (the passkey vault's blob shape), ADR-0054 (login handle,
stand-ins, limits), ADR-0064, ADR-0065 (fresh-auth grant, key reset),
ADR-0066 (recall anchor, rotation).
